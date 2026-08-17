use std::future::Future;

use axum::{
    body::Bytes,
    extract::Query,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};

use crate::config::CONFIG;

#[derive(serde::Deserialize)]
struct PrintQuery {
    /// 覆盖配置里的默认打印机
    printer: Option<String>,
}

fn router() -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/print", post(print_handler))
}

/// POST /print：body 为标签 PNG 原始字节；打印耗时约 1 秒（PowerShell 启动），
/// 放进阻塞线程池，完成前不占用异步 worker
async fn print_handler(Query(query): Query<PrintQuery>, body: Bytes) -> impl IntoResponse {
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "空图片数据".to_string());
    }
    let printer = query
        .printer
        .unwrap_or_else(|| CONFIG.print.printer_name.clone());
    let result = tokio::task::spawn_blocking(move || crate::print::print_png(&body, &printer)).await;
    match result {
        Ok(Ok(())) => (StatusCode::OK, "已发送到打印机".to_string()),
        Ok(Err(err)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("打印失败: {err:#}")),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("打印任务执行异常: {err}"),
        ),
    }
}

/// 启动 HTTP 服务并阻塞运行，直到 shutdown 完成（服务停止信号或 Ctrl+C）
pub fn run(shutdown: impl Future<Output = ()> + Send + 'static) {
    let runtime = tokio::runtime::Runtime::new().expect("创建 tokio runtime 失败");
    runtime.block_on(async move {
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