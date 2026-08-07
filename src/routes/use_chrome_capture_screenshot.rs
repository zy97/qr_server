use actix_files::NamedFile;
use actix_web::{post, web, Responder};
use headless_chrome::{
    browser::default_executable, protocol::cdp::Page, Browser, LaunchOptions, Tab,
};
use image::{ Luma};
use qrcode::QrCode;
use serde::Serialize;
use std::{
    env,
    ffi::OsStr,
    fs::File,
    sync::{Arc, LazyLock},
};
use tera::{Context, Tera};
use tracing::info;

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

static CTAB: LazyLock<Arc<Tab>> = LazyLock::new(|| BROWSER.new_tab().unwrap());

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_label);
}

#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    // 整个图片渲染时间大致在600-700ms附近跳动
    let tab = CTAB.clone();
    for label in labels.0 {
        let code = QrCode::new(&label.qr_string)?;
        let infos = split_info(&label.qr_string, &label);
        let image = code.render::<Luma<u8>>().build();
        image.save("templates/chrome/qr.png")?;

        let mut result = File::create("templates/chrome/result.html")?;
        TEMPLATES.render_to(
            "template.html",
            &Context::from_serialize(&infos)?,
            &mut result,
        )?;

        let file_path = env::current_dir()?.join("templates/chrome/result.html");
        info!("11111");
        let viewport = tab
            .navigate_to(&format!("file:///{}", file_path.display()))?
            .wait_for_element("table")?
            .get_box_model()?
            .margin_viewport();
        info!("22222");
        let image_data = tab.capture_screenshot(
            Page::CaptureScreenshotFormatOption::Png,
            Some(75),
            Some(viewport),
            true,
        )?;
        info!("33333");

        std::fs::write("chrome_result.png", image_data)?;
    }

    Ok(NamedFile::open("chrome_result.png")?)
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
        descrpition: label.material_name.clone(),
        product_model: label.part_no.clone(),
    }
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
}
