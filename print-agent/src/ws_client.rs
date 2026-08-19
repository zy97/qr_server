//! 与 qr_service 的 WebSocket 长连接客户端。
//! 「连接即工位身份」：渲染请求经本连接上行，PNG 结果沿同一条连接下行，
//! qr_service 不需要知道任何工位地址。断线指数退避自动重连。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::config::CONFIG;

static NEXT_JOB: AtomicU64 = AtomicU64::new(1);
static CONNECTED: AtomicBool = AtomicBool::new(false);

/// qr_service 渲染成功下发的内容
pub struct RenderOutcome {
    pub png: Vec<u8>,
    /// 服务器生成的打印脚本（纸张/打印机已按工位设置编入）
    pub script: String,
    /// 实际生效的打印机名（空 = 打印脚本运行时取系统默认打印机）
    pub printer: String,
    /// 打印机来源：server=服务器工位设置覆盖，agent=代理本地配置/请求参数
    pub printer_source: String,
}

struct ClientState {
    /// 序列化后的上行消息，由连接任务取走发送
    outbound: mpsc::UnboundedSender<String>,
    /// job_id → 等待渲染结果的通道（成功为 PNG + 服务器下发的打印脚本）
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<RenderOutcome, String>>>>>,
}

static CLIENT: LazyLock<ClientState> = LazyLock::new(|| {
    let (outbound, mut rx) = mpsc::unbounded_channel::<String>();
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<RenderOutcome, String>>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // 连接监督任务：需要先把 rx 挪进独立任务，等 server::run 的 runtime 起来后由 start() 触发
    let pending_for_task = pending.clone();
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            match run_connection(&mut rx, pending_for_task.clone()).await {
                Ok(()) => tracing::warn!("与 qr_service 的连接已断开"),
                Err(err) => tracing::warn!(error = %err, "连接 qr_service 失败"),
            }
            CONNECTED.store(false, Ordering::Relaxed);
            fail_all_pending(&pending_for_task, "与 qr_service 的连接已断开");
            tracing::info!(secs = backoff.as_secs(), "等待后重连 qr_service");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });

    ClientState { outbound, pending }
});

fn fail_all_pending(
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<Result<RenderOutcome, String>>>>>,
    reason: &str,
) {
    let mut pending = pending.lock().expect("pending poisoned");
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(reason.to_string()));
    }
}

/// 由 server::run 在 tokio runtime 内调用，启动连接监督任务
pub fn start() {
    LazyLock::force(&CLIENT);
}

pub fn is_connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

async fn run_connection(
    rx: &mut mpsc::UnboundedReceiver<String>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<RenderOutcome, String>>>>>,
) -> anyhow::Result<()> {
    let mac = local_mac_address().unwrap_or_default();
    let url = format!("{}?station={}&mac={}", CONFIG.server.url, CONFIG.server.station, mac);
    // tokio-tungstenite 0.26 的 connect_async 接受 &str
    let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
    let (mut sink, mut stream) = ws.split();
    // 接入即上报本机可用打印机列表，服务器代理管理页面据此提供下拉选择
    let mut known_printers = local_printers();
    let hello = json!({
        "type": "hello",
        "station": CONFIG.server.station,
        "printers": known_printers,
    })
    .to_string();
    sink.send(Message::Text(hello.into())).await?;
    CONNECTED.store(true, Ordering::Relaxed);
    tracing::info!("已连接 qr_service");
    // 连接存续期间定时刷新打印机列表：工位新增/删除打印机无需重启服务。
    // 每次刷新要起一个 powershell 进程跑 Get-Printer，10s 兼顾响应速度与进程开销；
    // 有变化才重新上报，避免给服务器刷无效消息
    let mut refresh = tokio::time::interval(Duration::from_secs(10));
    refresh.tick().await; // 跳过首次立即触发（刚在接入时上报过）

    loop {
        tokio::select! {
            _ = refresh.tick() => {
                let printers = tokio::task::spawn_blocking(local_printers)
                    .await
                    .unwrap_or_default();
                if printers != known_printers {
                    known_printers = printers.clone();
                    let hello = json!({
                        "type": "hello",
                        "station": CONFIG.server.station,
                        "printers": printers,
                    })
                    .to_string();
                    if sink.send(Message::Text(hello.into())).await.is_err() { break; }
                    tracing::info!(count = known_printers.len(), "打印机列表已更新并重新上报");
                }
            }
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => route_response(&text, &pending),
                Some(Ok(Message::Ping(bytes))) => {
                    if sink.send(Message::Pong(bytes)).await.is_err() { break; }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(err)) => return Err(err.into()),
                _ => {}
            },
            outgoing = rx.recv() => match outgoing {
                Some(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() { break; }
                }
                None => break,
            },
        }
    }
    Ok(())
}

#[cfg(windows)]
fn local_mac_address() -> Option<String> {
    let output = std::process::Command::new("getmac")
        .args(["/fo", "csv", "/nh"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split(',').next())
        .map(|value| value.trim().trim_matches('"'))
        .find(|value| value.len() == 17 && *value != "N/A")
        .map(str::to_string)
}

#[cfg(not(windows))]
fn local_mac_address() -> Option<String> {
    None
}

/// 本机可用打印机名列表（Windows 打印工位）。枚举失败返回空列表，不影响连接
#[cfg(windows)]
fn local_printers() -> Vec<String> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            // 强制 UTF-8 输出，避免中文打印机名在 GBK 控制台下乱码
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-Printer | Select-Object -ExpandProperty Name",
        ])
        .output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(not(windows))]
fn local_printers() -> Vec<String> {
    Vec::new()
}
fn route_response(
    text: &str,
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<Result<RenderOutcome, String>>>>>,
) {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };
    let job_id = value["job_id"].as_str().unwrap_or("").to_string();
    let result = match value["type"].as_str() {
        Some("render_ok") => {
            let png = value["png_base64"].as_str().and_then(|s| {
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s).ok()
            });
            let script = value["print_script"].as_str().map(|s| s.to_string());
            match (png, script) {
                (Some(png), Some(script)) => Ok(RenderOutcome {
                    png,
                    script,
                    printer: value["printer"].as_str().unwrap_or("").to_string(),
                    printer_source: value["printer_source"].as_str().unwrap_or("agent").to_string(),
                }),
                _ => Err("渲染结果缺少 png_base64 或 print_script".to_string()),
            }
        }
        Some("render_err") => Err(value["error"].as_str().unwrap_or("渲染失败").to_string()),
        _ => return,
    };
    let tx = pending.lock().expect("pending poisoned").remove(&job_id);
    if let Some(tx) = tx {
        let _ = tx.send(result);
    }
}

/// 请求 qr_service 渲染标签，返回 (job_id, 渲染结果)。
/// 渲染超时 30 秒；连接断开/未连接时立即报错
pub async fn render(
    labels: Vec<serde_json::Value>,
    template: Option<String>,
    printer: String,
) -> Result<(String, RenderOutcome), String> {
    if !is_connected() {
        return Err(format!(
            "未连接到 qr_service（{}），请检查服务器地址与网络",
            CONFIG.server.url
        ));
    }
    let job_id = format!(
        "{}-{}",
        std::process::id(),
        NEXT_JOB.fetch_add(1, Ordering::Relaxed)
    );
    let (tx, rx) = oneshot::channel();
    CLIENT
        .pending
        .lock()
        .expect("pending poisoned")
        .insert(job_id.clone(), tx);
    let message = json!({
        "type": "render",
        "job_id": job_id,
        "template": template,
        "labels": labels,
        "printer": printer,
    })
    .to_string();
    CLIENT
        .outbound
        .send(message)
        .map_err(|_| "连接任务已停止".to_string())?;

    match tokio::time::timeout(Duration::from_secs(30), rx).await {
        Ok(Ok(Ok(outcome))) => Ok((job_id, outcome)),
        Ok(Ok(Err(err))) => Err(err),
        Ok(Err(_)) => Err("渲染结果通道已关闭".to_string()),
        Err(_) => {
            CLIENT
                .pending
                .lock()
                .expect("pending poisoned")
                .remove(&job_id);
            Err("渲染超时（30 秒）".to_string())
        }
    }
}

/// 打印结果经 WS 上报 qr_service（异步打印完成后调用）。
/// 连接断开时只记日志不报错：本地打印已经发生，上报丢失不影响工位
pub fn notify_print_result(job_id: &str, printer: &str, result: Result<(), String>) {
    let ok = result.is_ok();
    let message = json!({
        "type": "print_result",
        "job_id": job_id,
        "printer": printer,
        "ok": ok,
        "error": result.err(),
    })
    .to_string();
    if CLIENT.outbound.send(message).is_err() {
        tracing::warn!(job_id, ok, "打印结果上报失败：连接任务已停止");
    }
}
