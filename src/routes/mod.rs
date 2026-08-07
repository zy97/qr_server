use actix_web::web;

#[cfg(feature = "master")]
pub mod master;
#[cfg(feature = "typst")]
pub mod typst;
#[cfg(feature = "chrome")]
pub mod use_chrome_capture_screenshot;

#[cfg(not(any(feature = "master", feature = "typst", feature = "chrome")))]
compile_error!("Enable exactly one implementation feature: `master`, `typst`, or `chrome`.");

#[cfg(any(
    all(feature = "master", feature = "typst"),
    all(feature = "master", feature = "chrome"),
    all(feature = "typst", feature = "chrome"),
))]
compile_error!("Only one implementation feature can be enabled at a time.");

pub fn configure(cfg: &mut web::ServiceConfig) {
    #[cfg(feature = "master")]
    master::configure(cfg);
    #[cfg(feature = "typst")]
    typst::configure(cfg);
    #[cfg(feature = "chrome")]
    use_chrome_capture_screenshot::configure(cfg);
}
