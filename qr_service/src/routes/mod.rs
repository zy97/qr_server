use actix_web::web;

#[cfg(feature = "agent-browser")]
pub mod agent_browser;
#[cfg(feature = "typst")]
pub mod typst;
#[cfg(feature = "chrome")]
pub mod use_chrome_capture_screenshot;

#[cfg(not(any(feature = "typst", feature = "chrome", feature = "agent-browser")))]
compile_error!("Enable exactly one implementation feature: `typst`, `chrome`, or `agent-browser`.");

#[cfg(any(
    all(feature = "typst", feature = "chrome"),
    all(feature = "typst", feature = "agent-browser"),
    all(feature = "chrome", feature = "agent-browser"),
))]
compile_error!("Only one implementation feature can be enabled at a time.");

pub fn configure(cfg: &mut web::ServiceConfig) {
    #[cfg(feature = "typst")]
    typst::configure(cfg);
    #[cfg(feature = "chrome")]
    use_chrome_capture_screenshot::configure(cfg);
    #[cfg(feature = "agent-browser")]
    agent_browser::configure(cfg);
}
/// 当前编译特性对应的渲染实现，供 /ws/agent 的渲染请求调用
pub async fn render_labels(
    labels: &[serde_json::Value],
    template: Option<&str>,
) -> Result<Vec<u8>, crate::err::CustomError> {
    #[cfg(feature = "typst")]
    return typst::render_labels(labels, template).await;
    #[cfg(feature = "chrome")]
    return use_chrome_capture_screenshot::render_labels(labels, template).await;
    #[cfg(feature = "agent-browser")]
    return agent_browser::render_labels(labels, template).await;
}
