//! print-agent 的 WebSocket 接入端。
//! 拓扑：工位浏览器 → 本机 print-agent →（WS 长连接）→ 本端点渲染 → 同一条连接返回 PNG。
//! agent 主动外连，「连接即工位身份」：服务器不需要配置任何工位地址，
//! 新增工位只需要在工位上装 print-agent 并填服务器地址。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use actix_web::{get, web, HttpRequest, HttpResponse};
use actix_ws::AggregatedMessage;
use base64::{engine::general_purpose, Engine};
use serde::Deserialize;

use crate::err::CustomError;

/// agent → server 的渲染请求
#[derive(Deserialize)]
#[serde(tag = "type")]
enum AgentMessage {
    #[serde(rename = "render")]
    Render {
        job_id: String,
        template: Option<String>,
        labels: Vec<serde_json::Value>,
        /// 生效的打印机名（agent 侧配置或请求覆盖）；空则打印到系统默认打印机
        printer: Option<String>,
    },
}

/// 渲染成功：除 PNG 外一并下发打印脚本（打印逻辑集中在服务器维护，见 print_script）
fn render_ok(job_id: &str, png: &[u8], printer: Option<&str>) -> String {
    serde_json::json!({
        "type": "render_ok",
        "job_id": job_id,
        "png_base64": general_purpose::STANDARD.encode(png),
        "print_script": crate::print_script::build_print_script(printer.unwrap_or("")),
    })
    .to_string()
}

fn render_err(job_id: &str, error: &str) -> String {
    serde_json::json!({
        "type": "render_err",
        "job_id": job_id,
        "error": error,
    })
    .to_string()
}

/// 在线工位注册表：station → (连接 id, 接入时间)。
/// 同一工位重连时旧连接的清理不能误删新连接，所以带上连接 id 比对
struct Registry {
    agents: HashMap<String, (u64, Instant)>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| {
    Mutex::new(Registry {
        agents: HashMap::new(),
    })
});

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

fn registry_add(station: &str, conn_id: u64) {
    REGISTRY
        .lock()
        .expect("registry poisoned")
        .agents
        .insert(station.to_string(), (conn_id, Instant::now()));
}

fn registry_remove(station: &str, conn_id: u64) {
    let mut registry = REGISTRY.lock().expect("registry poisoned");
    if matches!(registry.agents.get(station), Some((id, _)) if *id == conn_id) {
        registry.agents.remove(station);
    }
}

async fn handle_agent_message(text: &str) -> Option<String> {
    let message: AgentMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(error = %err, "无法解析的 agent 消息");
            return None;
        }
    };
    match message {
        AgentMessage::Render {
            job_id,
            template,
            labels,
            printer,
        } => match crate::routes::render_labels(&labels, template.as_deref()).await {
            Ok(png) => Some(render_ok(&job_id, &png, printer.as_deref())),
            Err(err) => Some(render_err(&job_id, &err.to_string())),
        },
    }
}

#[derive(Deserialize)]
struct AgentConnQuery {
    station: Option<String>,
}

#[get("/ws/agent")]
async fn agent_ws(
    req: HttpRequest,
    stream: web::Payload,
    query: web::Query<AgentConnQuery>,
) -> Result<HttpResponse, CustomError> {
    let station = query
        .station
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);

    let (response, mut session, stream) = actix_ws::handle(&req, stream)
        .map_err(|err| CustomError::OtherLibraryError(format!("ws 握手失败: {err}")))?;
    // PNG 的 base64 可能达到数百 KB，放宽聚合消息大小上限
    let mut stream = stream.aggregate_continuations().max_continuation_size(8 * 1024 * 1024);

    registry_add(&station, conn_id);
    tracing::info!(station, conn_id, "print-agent 已接入");

    actix_web::rt::spawn(async move {
        while let Some(Ok(message)) = stream.recv().await {
            match message {
                AggregatedMessage::Text(text) => {
                    if let Some(reply) = handle_agent_message(&text).await {
                        if session.text(reply).await.is_err() {
                            break;
                        }
                    }
                }
                AggregatedMessage::Ping(bytes) => {
                    let _ = session.pong(&bytes).await;
                }
                AggregatedMessage::Close(_) => break,
                _ => {}
            }
        }
        registry_remove(&station, conn_id);
        tracing::info!(station, conn_id, "print-agent 已断开");
    });

    Ok(response)
}

/// 在线工位列表（运维查看用）
#[get("/api/agents")]
async fn list_agents() -> HttpResponse {
    let registry = REGISTRY.lock().expect("registry poisoned");
    let agents: Vec<_> = registry
        .agents
        .iter()
        .map(|(station, (_, since))| {
            serde_json::json!({
                "station": station,
                "connected_secs": since.elapsed().as_secs(),
            })
        })
        .collect();
    HttpResponse::Ok().json(agents)
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(agent_ws).service(list_agents);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_render_request() {
        let message: AgentMessage = serde_json::from_str(
            r#"{"type":"render","job_id":"1-7","template":"9","labels":[{"part_no":"P-001"}],"printer":"P1"}"#,
        )
        .unwrap();
        match message {
            AgentMessage::Render {
                job_id,
                template,
                labels,
                printer,
            } => {
                assert_eq!(job_id, "1-7");
                assert_eq!(template.as_deref(), Some("9"));
                assert_eq!(labels.len(), 1);
                assert_eq!(printer.as_deref(), Some("P1"));
            }
        }
    }

    #[test]
    fn render_ok_carries_base64_png() {
        let text = render_ok("1-7", b"png-bytes", Some("ZDesigner ZT231-300dpi ZPL"));
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "render_ok");
        assert_eq!(value["job_id"], "1-7");
        assert!(value["print_script"].as_str().unwrap().contains("Get-PrintJob"));
        assert_eq!(
            general_purpose::STANDARD
                .decode(value["png_base64"].as_str().unwrap())
                .unwrap(),
            b"png-bytes"
        );
    }

    #[test]
    fn render_err_carries_message() {
        let text = render_err("1-7", "模板不存在");
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "render_err");
        assert_eq!(value["error"], "模板不存在");
    }

    #[test]
    fn registry_remove_only_matching_connection() {
        registry_add("STATION-A", 1);
        registry_add("STATION-A", 2); // 同工位重连，新连接覆盖
        registry_remove("STATION-A", 1); // 旧连接断开，不应误删
        assert!(REGISTRY.lock().unwrap().agents.contains_key("STATION-A"));
        registry_remove("STATION-A", 2);
        assert!(!REGISTRY.lock().unwrap().agents.contains_key("STATION-A"));
    }
}