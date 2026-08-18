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

struct ClientState {
    /// 序列化后的上行消息，由连接任务取走发送
    outbound: mpsc::UnboundedSender<String>,
    /// job_id → 等待渲染结果的通道（成功为 PNG + 服务器下发的打印脚本）
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<(Vec<u8>, String), String>>>>>,
}

static CLIENT: LazyLock<ClientState> = LazyLock::new(|| {
    let (outbound, mut rx) = mpsc::unbounded_channel::<String>();
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<(Vec<u8>, String), String>>>>> =
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
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<Result<(Vec<u8>, String), String>>>>>,
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
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Result<(Vec<u8>, String), String>>>>>,
) -> anyhow::Result<()> {
    let mac = local_mac_address().unwrap_or_default();
    let url = format!("{}?station={}&mac={}", CONFIG.server.url, CONFIG.server.station, mac);
    // tokio-tungstenite 0.26 的 connect_async 接受 &str
    let (ws, _) = tokio_tungstenite::connect_async(&url).await?;
    let (mut sink, mut stream) = ws.split();
    CONNECTED.store(true, Ordering::Relaxed);
    tracing::info!("已连接 qr_service");

    loop {
        tokio::select! {
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
fn route_response(
    text: &str,
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<Result<(Vec<u8>, String), String>>>>>,
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
                (Some(png), Some(script)) => Ok((png, script)),
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

/// 请求 qr_service 渲染标签，返回 PNG 字节。
/// 渲染超时 30 秒；连接断开/未连接时立即报错
pub async fn render(
    labels: Vec<serde_json::Value>,
    template: Option<String>,
    printer: String,
) -> Result<(Vec<u8>, String), String> {
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
        Ok(Ok(result)) => result,
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
