use actix_web::{post, web, HttpResponse, Responder};
use base64::{engine::general_purpose, Engine};
use headless_chrome::{protocol::cdp::Page, Browser, Tab};
use image::{DynamicImage, ImageFormat, Luma};
use qrcode::QrCode;
use serde::Serialize;
use std::{
    io::Cursor,
    sync::{Arc, LazyLock, Mutex},
};
use tera::{Context, Tera};

use crate::err::CustomError;
use crate::requests::dtos::create_lable_dto::LabelInfo;

static TEMPLATES: LazyLock<Tera> = LazyLock::new(|| {
    let mut tera = Tera::new();
    tera.add_template_file("templates/template.html", Some("template.html"))
        .expect("failed to load master template");
    tera.autoescape_on(vec![".html", ".sql"]);
    tera
});

static BROWSER: LazyLock<Browser> = LazyLock::new(|| Browser::default().unwrap());
static CTAB: LazyLock<Mutex<Arc<Tab>>> = LazyLock::new(|| Mutex::new(BROWSER.new_tab().unwrap()));

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_label);
}

#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    // 整个图片渲染时间大致在800-900ms附近跳动
    let tab = CTAB
        .lock()
        .map_err(|_| CustomError::OtherLibraryError("master chrome tab lock poisoned".into()))?;
    let mut result_image = None;

    for label in labels.0 {
        let mut infos = split_info(&label.qr_string);
        infos.qr_code = Some(qr_code_data_uri(&label.qr_string)?);

        let rendered = TEMPLATES.render("template.html", &Context::from_serialize(&infos)?)?;
        let viewport = tab
            .navigate_to(&html_data_url(&rendered))?
            .wait_for_element("#app")?
            .get_box_model()?
            .content_viewport();
        let image_data = tab.capture_screenshot(
            Page::CaptureScreenshotFormatOption::Png,
            Some(75),
            Some(viewport),
            true,
        )?;

        result_image = Some(image_data);
    }

    let result_image = result_image
        .ok_or_else(|| CustomError::OtherLibraryError("no label data provided".to_string()))?;

    Ok(HttpResponse::Ok()
        .content_type("image/png")
        .body(result_image))
}

fn split_info(code: &str) -> TemplateData {
    let infos = code.split('|').collect::<Vec<&str>>();
    TemplateData {
        material_no: infos[0].to_string(),
        lot_no: infos[1].to_string(),
        order_no: infos[2].to_string(),
        count: infos[3].to_string(),
        vender_code: infos[4].to_string(),
        date: infos[5].to_string(),
        box_no: infos[6].to_string(),
        qr_code: None,
    }
}

fn qr_code_data_uri(content: &str) -> Result<String, CustomError> {
    let image = DynamicImage::ImageLuma8(QrCode::new(content)?.render::<Luma<u8>>().build());
    let mut png_bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)?;

    Ok(format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(png_bytes)
    ))
}

fn html_data_url(html: &str) -> String {
    format!(
        "data:text/html;base64,{}",
        general_purpose::STANDARD.encode(html)
    )
}

#[derive(Serialize)]
struct TemplateData {
    material_no: String,
    lot_no: String,
    order_no: String,
    count: String,
    vender_code: String,
    date: String,
    box_no: String,
    qr_code: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_template_without_html2canvas() {
        let template_data = split_info("M001|L001|O001|10|V001|2026-08-07|B001");
        let rendered = TEMPLATES
            .render(
                "template.html",
                &Context::from_serialize(&template_data).expect("serialize template data"),
            )
            .expect("master template should render");

        assert!(!rendered.contains("html2canvas"));
    }

    #[test]
    fn encodes_rendered_template_as_html_data_url() {
        let template_data = split_info("M001|L001|O001|10|V001|2026-08-07|B001");
        let rendered = TEMPLATES
            .render(
                "template.html",
                &Context::from_serialize(&template_data).expect("serialize template data"),
            )
            .expect("master template should render");
        let data_url = html_data_url(&rendered);
        let encoded = data_url
            .strip_prefix("data:text/html;base64,")
            .expect("template should use an HTML data URL");
        let decoded = general_purpose::STANDARD
            .decode(encoded)
            .expect("data URL should contain Base64 data");

        assert_eq!(decoded, rendered.as_bytes());
    }

    #[test]
    fn encodes_qr_code_as_png_data_uri_without_a_file() {
        let data_uri = qr_code_data_uri("M001|L001|O001|10|V001|2026-08-07|B001")
            .expect("QR code should encode");
        let encoded = data_uri
            .strip_prefix("data:image/png;base64,")
            .expect("QR code should use a PNG data URI");
        let png_bytes = general_purpose::STANDARD
            .decode(encoded)
            .expect("data URI should contain Base64 data");

        assert_eq!(&png_bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn rejects_qr_code_content_that_exceeds_capacity() {
        assert!(qr_code_data_uri(&"A".repeat(10_000)).is_err());
    }
}
