use actix_web::web;

pub mod master;
pub mod typst;
pub mod use_chrome_capture_screenshot;

pub fn configure(cfg: &mut web::ServiceConfig) {
    master::configure(cfg);
    typst::configure(cfg);
    use_chrome_capture_screenshot::configure(cfg);
}
