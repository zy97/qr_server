use actix_web::web;

#[cfg(feature = "agent-browser")]
pub mod agent_browser;
#[cfg(feature = "master")]
pub mod master;
#[cfg(feature = "typst")]
pub mod typst;
#[cfg(feature = "chrome")]
pub mod use_chrome_capture_screenshot;

#[cfg(not(any(
    feature = "master",
    feature = "typst",
    feature = "chrome",
    feature = "agent-browser"
)))]
compile_error!(
    "Enable exactly one implementation feature: `master`, `typst`, `chrome`, or `agent-browser`."
);

#[cfg(any(
    all(feature = "master", feature = "typst"),
    all(feature = "master", feature = "chrome"),
    all(feature = "master", feature = "agent-browser"),
    all(feature = "typst", feature = "chrome"),
    all(feature = "typst", feature = "agent-browser"),
    all(feature = "chrome", feature = "agent-browser"),
))]
compile_error!("Only one implementation feature can be enabled at a time.");

pub fn configure(cfg: &mut web::ServiceConfig) {
    #[cfg(feature = "master")]
    master::configure(cfg);
    #[cfg(feature = "typst")]
    typst::configure(cfg);
    #[cfg(feature = "chrome")]
    use_chrome_capture_screenshot::configure(cfg);
    #[cfg(feature = "agent-browser")]
    agent_browser::configure(cfg);
}
