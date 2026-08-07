use actix_files::NamedFile;
use actix_web::{post, web, Responder};
use base64::{engine::general_purpose, Engine};
use image::{ImageFormat, ImageReader, Luma};
use qrcode::QrCode;
use serde::Serialize;
use std::{env, fs, io::Cursor, process::Command, sync::LazyLock};
use tera::{Context, Tera};
use tracing::info;

use crate::err::CustomError;
use crate::requests::dtos::create_lable_dto::LabelInfo;

static TEMPLATES: LazyLock<Tera> = LazyLock::new(|| {
    let mut tera = Tera::new();
    tera.add_template_file(
        "templates/agent-browser-template.html",
        Some("template.html"),
    )
    .expect("failed to load agent-browser template");
    tera.autoescape_on(vec![".html", ".sql"]);
    tera
});

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_label);
}

#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    // 整个图片渲染时间大致在200-300ms附近跳动
    let current_dir = env::current_dir()?;
    let agent_browser_dir = current_dir.join("templates/agent-browser");
    fs::create_dir_all(&agent_browser_dir)?;
    let template_path = agent_browser_dir.join("result.html");
    let qr_path = agent_browser_dir.join("qr.png");
    let output_path = current_dir.join("agent_browser_result.png");
    let agent_browser_path = current_dir.join("agent-browser.exe");

    for label in labels.0 {
        let code = QrCode::new(&label.qr_string)?;
        let mut infos = split_info(&label.qr_string);
        let image = code.render::<Luma<u8>>().build();
        image.save(&qr_path)?;

        let img = ImageReader::open(&qr_path)?.decode()?;
        let mut img_bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut img_bytes), ImageFormat::Png)?;
        let base64_string = general_purpose::STANDARD.encode(&img_bytes);
        infos.qr_code = Some(format!("data:image/png;base64,{}", base64_string));

        let rendered = TEMPLATES.render("template.html", &Context::from_serialize(&infos)?)?;
        fs::write(&template_path, rendered)?;

        let status = Command::new(&agent_browser_path)
            .arg("--session")
            .arg("qr-service-agent-browser")
            .arg("--allow-file-access")
            .arg("batch")
            .arg("--bail")
            .arg("set viewport 1200 800")
            .arg(format!("open {}", file_url(&template_path)))
            .arg("wait #app")
            .arg(format!("screenshot #app {}", command_path(&output_path)))
            .status()
            .map_err(|error| {
                CustomError::OtherLibraryError(format!(
                    "failed to start agent-browser at {}: {error}",
                    agent_browser_path.display()
                ))
            })?;

        if !status.success() {
            return Err(CustomError::OtherLibraryError(format!(
                "agent-browser failed to render {} with status {status}",
                template_path.display()
            )));
        }

        info!("agent-browser rendered {}", output_path.display());
    }

    Ok(NamedFile::open(output_path)?)
}

fn file_url(path: &std::path::Path) -> String {
    format!("file:///{}", command_path(path))
}

fn command_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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
    fn renders_template_with_agent_browser_template_data() {
        let template_data = split_info("M001|L001|O001|10|V001|2026-08-07|B001");
        let context = Context::from_serialize(&template_data).expect("serialize template data");

        TEMPLATES
            .render("template.html", &context)
            .expect("agent-browser template should render with master template data");
    }
}
