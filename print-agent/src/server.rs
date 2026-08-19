use std::future::Future;

use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use tower_http::cors::{AllowPrivateNetwork, CorsLayer};

use crate::config::CONFIG;

#[derive(serde::Deserialize)]
struct LabelQuery {
    /// 指定渲染的模板（id 或名称），缺省用默认模板
    template: Option<String>,
    /// 覆盖配置里的默认打印机
    printer: Option<String>,
}

fn router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/label", post(label_handler))
        // 业务页面（http://<服务器>/...）跨域调本机 agent，放开 CORS；
        // allow_private_network：Chrome 私网访问规则（PNA）下，内网/HTTPS 页面
        // 调 127.0.0.1 需要预检响应带 Access-Control-Allow-Private-Network
        .layer(CorsLayer::permissive().allow_private_network(AllowPrivateNetwork::yes()))
}


async fn health() -> String {
    if crate::ws_client::is_connected() {
        "ok, qr_service 已连接".to_string()
    } else {
        "ok, qr_service 未连接".to_string()
    }
}

/// POST /label（工位浏览器调用）：body 为标签数据 JSON 数组。
/// 链路：浏览器 → 本端点 → WS 转发 qr_service 渲染 → 立即响应标签 PNG，
/// 打印在后台异步执行，真实结果（含队列监听）经 WS 上报 qr_service
async fn label_handler(
    Query(query): Query<LabelQuery>,
    Json(labels): Json<serde_json::Value>,
) -> Response {
    let labels = match labels.as_array() {
        Some(arr) if !arr.is_empty() => arr.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "请求体应为非空标签数据数组".to_string(),
            )
                .into_response()
        }
    };
    let labels_count = labels.len();
    // 打印机名随渲染请求上行，qr_service 把它编进下发的打印脚本
    let printer = query
        .printer
        .unwrap_or_else(|| CONFIG.print.printer_name.clone());
    let (job_id, outcome) = match crate::ws_client::render(labels, query.template, printer).await {
        Ok(ok) => ok,
        Err(err) => return (StatusCode::BAD_GATEWAY, format!("渲染失败: {err}")).into_response(),
    };
    let crate::ws_client::RenderOutcome {
        png,
        script,
        printer,
        printer_source,
    } = outcome;
    // 渲染成功即响应；打印在后台执行，不阻塞工位浏览器。
    // 打印结果经 WS 上报 qr_service 日志（同 job_id 对账），本机日志同样记录
    tracing::info!(
        job_id,
        labels = labels_count,
        printer,
        printer_source,
        "已受理打印任务，转入后台打印"
    );
    let png_for_print = png.clone();
    tokio::task::spawn_blocking(move || {
        let result = crate::print::print_with_script(&script, &png_for_print)
            .map_err(|err| format!("{err:#}"));
        match &result {
            Ok(()) => tracing::info!(job_id, printer, "打印完成"),
            Err(err) => tracing::warn!(job_id, printer, error = %err, "打印失败"),
        }
        crate::ws_client::notify_print_result(&job_id, &printer, result);
    });
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "image/png")],
        png,
    )
        .into_response()
}

/// 启动 HTTP 服务并阻塞运行，直到 shutdown 完成（服务停止信号或 Ctrl+C）
pub fn run(shutdown: impl Future<Output = ()> + Send + 'static) {
    let runtime = tokio::runtime::Runtime::new().expect("创建 tokio runtime 失败");
    runtime.block_on(async move {
        // 先拉起与 qr_service 的 WS 长连接（断线自动重连）
        crate::ws_client::start();
        if CONFIG.server.url.is_empty() {
            tracing::warn!("未配置 server.url（qr_service 地址），/label 将不可用");
        }
        let addr = format!("0.0.0.0:{}", CONFIG.server.port);
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .unwrap_or_else(|err| panic!("监听 {addr} 失败: {err}"));
        tracing::info!(addr, "print-agent 已启动");
        axum::serve(listener, router())
            .with_graceful_shutdown(shutdown)
            .await
            .expect("HTTP 服务运行失败");
    });
}
