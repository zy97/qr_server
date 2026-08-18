//! print-agent 的 WebSocket 接入端。
//! 拓扑：工位浏览器 → 本机 print-agent →（WS 长连接）→ 本端点渲染 → 同一条连接返回 PNG。
//! agent 主动外连，「连接即工位身份」：服务器不需要配置任何工位地址，
//! 新增工位只需要在工位上装 print-agent 并填服务器地址。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use actix_web::{delete, get, put, web, HttpRequest, HttpResponse};
use actix_ws::AggregatedMessage;
use base64::{engine::general_purpose, Engine};
use serde::Deserialize;

use crate::err::CustomError;
use crate::template_store as store;

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
    /// 接入后上报的本机信息（可用打印机列表等）
    #[serde(rename = "hello")]
    Hello { printers: Vec<String> },
    /// agent 异步打印完成后上报的结果（/label 已在渲染后即响应，不等打印）
    #[serde(rename = "print_result")]
    PrintResult {
        job_id: String,
        ok: bool,
        error: Option<String>,
    },
}

/// 渲染成功：除 PNG 外一并下发打印脚本（打印逻辑集中在服务器维护，见 print_script）。
/// 打印机与纸张由工位级设置覆盖；未覆盖字段回退代理本地配置 / 服务器全局配置
fn render_ok(job_id: &str, png: &[u8], printer: Option<&str>, settings: &store::AgentSettings) -> String {
    serde_json::json!({
        "type": "render_ok",
        "job_id": job_id,
        "png_base64": general_purpose::STANDARD.encode(png),
        "print_script": crate::print_script::build_print_script(
            printer.unwrap_or(""),
            settings.paper_width,
            settings.paper_height,
        ),
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

/// 在线工位连接信息
struct AgentConnection {
    conn_id: u64,
    connected_at: Instant,
    /// 服务器观察到的对端 IP（WebSocket 对端地址）
    ip: String,
    /// 客户端自报的 MAC 地址；旧版本客户端不带该参数则为空
    mac: Option<String>,
    /// 客户端接入时上报的本机可用打印机列表；旧版本客户端不上报则为空
    printers: Vec<String>,
}

/// 在线工位注册表：station → 连接信息。
/// 同一工位重连时旧连接的清理不能误删新连接，所以带上连接 id 比对
struct Registry {
    agents: HashMap<String, AgentConnection>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| {
    Mutex::new(Registry {
        agents: HashMap::new(),
    })
});

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

fn registry_add(station: &str, conn_id: u64, ip: String, mac: Option<String>) {
    REGISTRY
        .lock()
        .expect("registry poisoned")
        .agents
        .insert(
            station.to_string(),
            AgentConnection {
                conn_id,
                connected_at: Instant::now(),
                ip,
                mac,
                printers: Vec::new(),
            },
        );
}

/// 更新连接上报的打印机列表（带 conn_id 比对，同工位重连时旧连接的消息不能覆盖新连接）
fn registry_set_printers(station: &str, conn_id: u64, printers: Vec<String>) {
    let mut registry = REGISTRY.lock().expect("registry poisoned");
    if let Some(agent) = registry.agents.get_mut(station) {
        if agent.conn_id == conn_id {
            agent.printers = printers;
        }
    }
}

fn registry_remove(station: &str, conn_id: u64) {
    let mut registry = REGISTRY.lock().expect("registry poisoned");
    if matches!(registry.agents.get(station), Some(agent) if agent.conn_id == conn_id) {
        registry.agents.remove(station);
    }
}

async fn handle_agent_message(text: &str, station: &str, conn_id: u64) -> Option<String> {
    let message: AgentMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(error = %err, "无法解析的 agent 消息");
            return None;
        }
    };
    match message {
        AgentMessage::Hello { printers } => {
            registry_set_printers(station, conn_id, printers);
            None
        }
        AgentMessage::PrintResult { job_id, ok, error } => {
            if ok {
                tracing::info!(station, job_id, "打印完成");
            } else {
                tracing::warn!(station, job_id, error, "打印失败");
            }
            None
        }
        AgentMessage::Render {
            job_id,
            template,
            labels,
            printer,
        } => {
            // 代理管理页面可为工位指定打印机/纸张；未覆盖的字段用代理本地配置
            let settings = agent_settings(station).unwrap_or_default();
            let printer = settings.printer.clone().or(printer);
            match crate::routes::render_labels(&labels, template.as_deref()).await {
                Ok(png) => Some(render_ok(&job_id, &png, printer.as_deref(), &settings)),
                Err(err) => Some(render_err(&job_id, &err.to_string())),
            }
        }
    }
}

/// 服务器侧工位打印设置覆盖；读取失败时视为无覆盖，不阻断打印
fn agent_settings(station: &str) -> Option<store::AgentSettings> {
    match store::with_db(|conn| store::get_agent_settings(conn, station)) {
        Ok(settings) => settings,
        Err(err) => {
            tracing::warn!(station, error = %err, "读取工位打印设置失败，按未覆盖处理");
            None
        }
    }
}

#[derive(Deserialize)]
struct AgentConnQuery {
    station: Option<String>,
    mac: Option<String>,
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
    let ip = req
        .peer_addr()
        .map(|address| address.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let mac = query.mac.clone().filter(|value| !value.is_empty());

    let (response, mut session, stream) = actix_ws::handle(&req, stream)
        .map_err(|err| CustomError::OtherLibraryError(format!("ws 握手失败: {err}")))?;
    // PNG 的 base64 可能达到数百 KB，放宽聚合消息大小上限
    let mut stream = stream
        .aggregate_continuations()
        .max_continuation_size(8 * 1024 * 1024);

    registry_add(&station, conn_id, ip.clone(), mac.clone());
    tracing::info!(station, conn_id, ip, mac, "print-agent 已接入");

    actix_web::rt::spawn(async move {
        while let Some(Ok(message)) = stream.recv().await {
            match message {
                AggregatedMessage::Text(text) => {
                    if let Some(reply) = handle_agent_message(&text, &station, conn_id).await {
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
    // 先拷贝注册表快照再查覆盖设置，避免同时持有注册表锁与数据库锁
    let snapshot: Vec<_> = {
        let registry = REGISTRY.lock().expect("registry poisoned");
        registry
            .agents
            .iter()
            .map(|(station, agent)| {
                (
                    station.clone(),
                    agent.ip.clone(),
                    agent.mac.clone(),
                    agent.printers.clone(),
                    agent.connected_at.elapsed().as_secs(),
                )
            })
            .collect()
    };
    let agents: Vec<_> = snapshot
        .into_iter()
        .map(|(station, ip, mac, printers, secs)| {
            let settings = agent_settings(&station).unwrap_or_default();
            serde_json::json!({
                "station": station,
                "ip": ip,
                "mac": mac,
                "connected_secs": secs,
                "printers": printers,
                "printer_override": settings.printer,
                "paper_width": settings.paper_width,
                "paper_height": settings.paper_height,
            })
        })
        .collect();
    HttpResponse::Ok().json(agents)
}

#[derive(Deserialize)]
struct SetAgentSettingsRequest {
    printer: Option<String>,
    /// 自定义纸张宽度（cm）
    paper_width: Option<f64>,
    /// 自定义纸张高度（cm）
    paper_height: Option<f64>,
}

/// 设置/更新工位打印设置覆盖；空打印机名视为不覆盖打印机
#[put("/api/agents/{station}/settings")]
async fn set_agent_settings(
    path: web::Path<String>,
    body: web::Json<SetAgentSettingsRequest>,
) -> Result<HttpResponse, CustomError> {
    let station = path.into_inner();
    let settings = store::AgentSettings {
        printer: body
            .printer
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string),
        paper_width: body.paper_width,
        paper_height: body.paper_height,
    };
    store::with_db(|conn| store::set_agent_settings(conn, &station, &settings))?;
    Ok(HttpResponse::Ok().finish())
}

/// 清除工位打印设置覆盖，回退代理本地配置 + 服务器全局纸张
#[delete("/api/agents/{station}/settings")]
async fn clear_agent_settings(path: web::Path<String>) -> Result<HttpResponse, CustomError> {
    store::with_db(|conn| store::delete_agent_settings(conn, &path.into_inner()))?;
    Ok(HttpResponse::Ok().finish())
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(agent_ws)
        .service(list_agents)
        .service(set_agent_settings)
        .service(clear_agent_settings);
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
            _ => panic!("expected render"),
        }
    }

    #[test]
    fn parses_hello_and_updates_registry_printers() {
        let message: AgentMessage =
            serde_json::from_str(r#"{"type":"hello","printers":["ZDesigner ZT231","HP M403"]}"#)
                .unwrap();
        match message {
            AgentMessage::Hello { printers } => {
                assert_eq!(printers, vec!["ZDesigner ZT231", "HP M403"]);
            }
            _ => panic!("expected hello"),
        }
        registry_add("STATION-C", 9, "10.0.0.3".to_string(), None);
        registry_set_printers("STATION-C", 8, vec!["STALE".to_string()]); // 旧连接不能覆盖
        registry_set_printers("STATION-C", 9, vec!["P1".to_string(), "P2".to_string()]);
        assert_eq!(
            REGISTRY.lock().unwrap().agents["STATION-C"].printers,
            vec!["P1", "P2"]
        );
        registry_remove("STATION-C", 9);
    }
    #[test]
    fn parses_print_result() {
        let message: AgentMessage = serde_json::from_str(
            r#"{"type":"print_result","job_id":"1-7","ok":false,"error":"打印机脱机"}"#,
        )
        .unwrap();
        match message {
            AgentMessage::PrintResult { job_id, ok, error } => {
                assert_eq!(job_id, "1-7");
                assert!(!ok);
                assert_eq!(error.as_deref(), Some("打印机脱机"));
            }
            _ => panic!("expected print_result"),
        }
    }
    #[test]
    fn render_ok_carries_base64_png() {
        let text = render_ok("1-7", b"png-bytes", Some("ZDesigner ZT231-300dpi ZPL"), &store::AgentSettings::default());
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "render_ok");
        assert_eq!(value["job_id"], "1-7");
        assert!(value["print_script"]
            .as_str()
            .unwrap()
            .contains("Get-PrintJob"));
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
        registry_add("STATION-B", 1, "10.0.0.1".to_string(), None);
        registry_add(
            "STATION-B",
            2,
            "10.0.0.2".to_string(),
            Some("00-11-22-33-44-55".to_string()),
        ); // 同工位重连，新连接覆盖
        registry_remove("STATION-B", 1); // 旧连接断开，不应误删
        {
            let registry = REGISTRY.lock().unwrap();
            let agent = registry.agents.get("STATION-B").unwrap();
            assert_eq!(agent.ip, "10.0.0.2");
            assert_eq!(agent.mac.as_deref(), Some("00-11-22-33-44-55"));
        }
        registry_remove("STATION-B", 2);
        assert!(!REGISTRY.lock().unwrap().agents.contains_key("STATION-B"));
    }
}