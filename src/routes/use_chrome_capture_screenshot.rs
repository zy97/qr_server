use actix_web::{post, web, HttpResponse, Responder};
use base64::{engine::general_purpose, Engine};
use headless_chrome::{
    browser::default_executable, protocol::cdp::Page, Browser, LaunchOptions, Tab,
};
use image::{DynamicImage, ImageFormat, Luma};
use qrcode::QrCode;
use serde::Serialize;
use std::{
    ffi::OsStr,
    io::Cursor,
    sync::{Arc, LazyLock, Mutex},
};
use tera::{Context, Tera};

use crate::err::CustomError;
use crate::requests::dtos::create_lable_dto::LabelInfo;

static TEMPLATES: LazyLock<Tera> = LazyLock::new(|| {
    let mut tera = Tera::new();
    tera.add_template_file("templates/chrome/template.html", Some("template.html"))
        .expect("failed to load chrome template");
    tera.autoescape_on(vec![".html", ".sql"]);
    tera
});

static BROWSER: LazyLock<Browser> = LazyLock::new(|| {
    let launch_options = LaunchOptions::default_builder()
        .path(Some(default_executable().map_err(|e| e).unwrap()))
        .build()
        .unwrap();
    Browser::new(LaunchOptions {
        headless: true,
        args: vec![&OsStr::new("--disable-gpu")],
        ..launch_options
    })
    .unwrap()
});

static CTAB: LazyLock<Mutex<Arc<Tab>>> = LazyLock::new(|| Mutex::new(BROWSER.new_tab().unwrap()));

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_label);
}

#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    // 整个图片渲染时间大致在600-700ms附近跳动
    let tab = CTAB
        .lock()
        .map_err(|_| CustomError::OtherLibraryError("chrome tab lock poisoned".into()))?;
    let mut result_image = None;

    for label in labels.0 {
        let mut infos = split_info(&label.qr_string, &label);
        infos.base64 = Some(qr_code_data_uri(&label.qr_string)?);
        infos.logo_base64 = Some(file_data_uri("templates/logo.png", "image/png")?);

        let rendered = TEMPLATES.render("template.html", &Context::from_serialize(&infos)?)?;
        let viewport = tab
            .navigate_to(&html_data_url(&rendered))?
            .wait_for_element("table")?
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

fn split_info(code: &str, label: &LabelInfo) -> TemplateData {
    let infos = code.split('|').collect::<Vec<&str>>();
    TemplateData {
        material_no: infos[0].to_string(),
        lot_no: infos[1].to_string(),
        order_no: infos[2].to_string(),
        count: infos[3].to_string(),
        vender_code: infos[4].to_string(),
        date: infos[5].to_string(),
        box_no: infos[6].to_string(),
        customer_name: label.customer_name.clone(),
        base64: None,
        logo_base64: None,
        descrpition: label.material_name.clone(),
        product_model: label.part_no.clone(),
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

fn file_data_uri(path: &str, content_type: &str) -> Result<String, CustomError> {
    let bytes = std::fs::read(path)?;
    Ok(format!(
        "data:{content_type};base64,{}",
        general_purpose::STANDARD.encode(bytes)
    ))
}

fn html_data_url(html: &str) -> String {
    format!(
        "data:text/html;base64,{}",
        general_purpose::STANDARD.encode(html)
    )
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct TemplateData {
    material_no: String,
    lot_no: String,
    order_no: String,
    count: String,
    vender_code: String,
    date: String,
    box_no: String,
    customer_name: String,
    base64: Option<String>,
    logo_base64: Option<String>,
    descrpition: String,
    product_model: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_template_with_chrome_template_data() {
        let label = LabelInfo {
            kind: 1,
            customer_name: "客户".to_string(),
            part_no: "P-001".to_string(),
            material_name: "物料".to_string(),
            qr_string: "M001|L001|O001|10|V001|2026-08-07|B001".to_string(),
            is_return: false,
        };
        let template_data = split_info(&label.qr_string, &label);
        let context = Context::from_serialize(&template_data).expect("serialize template data");

        TEMPLATES
            .render("template.html", &context)
            .expect("chrome template should render with chrome template data");
    }

    #[test]
    fn renders_template_without_relative_image_files() {
        let label = LabelInfo {
            kind: 1,
            customer_name: "客户".to_string(),
            part_no: "P-001".to_string(),
            material_name: "物料".to_string(),
            qr_string: "M001|L001|O001|10|V001|2026-08-07|B001".to_string(),
            is_return: false,
        };
        let mut template_data = split_info(&label.qr_string, &label);
        template_data.base64 = Some(qr_code_data_uri(&label.qr_string).expect("encode QR code"));
        template_data.logo_base64 = Some("data:image/png;base64,logo".to_string());
        let rendered = TEMPLATES
            .render(
                "template.html",
                &Context::from_serialize(&template_data).expect("serialize template data"),
            )
            .expect("chrome template should render");

        assert!(!rendered.contains("../logo.png"));
        assert!(!rendered.contains("./qr.png"));
        assert!(rendered.contains("data:image/png;base64,"));
    }

    #[test]
    fn encodes_rendered_template_as_html_data_url() {
        let data_url = html_data_url("<table></table>");
        let encoded = data_url
            .strip_prefix("data:text/html;base64,")
            .expect("template should use an HTML data URL");
        let decoded = general_purpose::STANDARD
            .decode(encoded)
            .expect("data URL should contain Base64 data");

        assert_eq!(decoded, b"<table></table>");
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
}
