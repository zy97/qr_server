use actix_web::{post, web, HttpResponse, Responder};
use base64::{engine::general_purpose, Engine};
use image::{DynamicImage, ImageFormat, Luma};
use qrcode::QrCode;
use serde::Serialize;
use std::{
    env, fs,
    io::Cursor,
    path::PathBuf,
    process::{self, Command},
    sync::LazyLock,
    time::{SystemTime, UNIX_EPOCH},
};
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
    let agent_browser_path = current_dir.join("agent-browser.exe");
    let agent_browser_dir = agent_browser_path
        .parent()
        .ok_or_else(|| CustomError::OtherLibraryError("invalid agent-browser path".to_string()))?;
    let mut result_image = None;

    for label in labels.0 {
        let mut infos = split_info(&label.qr_string);
        infos.qr_code = Some(qr_code_data_uri(&label.qr_string)?);

        let rendered = TEMPLATES.render("template.html", &Context::from_serialize(&infos)?)?;
        let output_path = TempScreenshot::new(agent_browser_dir)?;

        let status = Command::new(&agent_browser_path)
            .arg("--session")
            .arg("qr-service-agent-browser")
            .arg("batch")
            .arg("--bail")
            .arg("set viewport 1200 800")
            .arg(format!("open {}", html_data_url(&rendered)))
            .arg("wait #app")
            .arg(format!(
                "screenshot #app {}",
                command_path(output_path.path())
            ))
            .status()
            .map_err(|error| {
                CustomError::OtherLibraryError(format!(
                    "failed to start agent-browser at {}: {error}",
                    agent_browser_path.display()
                ))
            })?;

        if !status.success() {
            return Err(CustomError::OtherLibraryError(format!(
                "agent-browser failed to render label with status {status}"
            )));
        }

        let image_data = fs::read(output_path.path())?;
        info!("agent-browser rendered {}", output_path.path().display());
        result_image = Some(image_data);
    }

    let result_image = result_image
        .ok_or_else(|| CustomError::OtherLibraryError("no label data provided".to_string()))?;

    Ok(HttpResponse::Ok()
        .content_type("image/png")
        .body(result_image))
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

struct TempScreenshot {
    path: PathBuf,
}

impl TempScreenshot {
    fn new(base_dir: &std::path::Path) -> Result<Self, CustomError> {
        let temp_dir = base_dir.join("temp");
        fs::create_dir_all(&temp_dir)?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let path = temp_dir.join(format!(
            "qr_service_agent_browser_{}_{}.png",
            process::id(),
            timestamp
        ));

        Ok(Self { path })
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempScreenshot {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
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

    #[test]
    fn encodes_rendered_template_as_html_data_url() {
        let data_url = html_data_url("<table id=\"app\"></table>");
        let encoded = data_url
            .strip_prefix("data:text/html;base64,")
            .expect("template should use an HTML data URL");
        let decoded = general_purpose::STANDARD
            .decode(encoded)
            .expect("data URL should contain Base64 data");

        assert_eq!(decoded, b"<table id=\"app\"></table>");
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
    fn renders_template_without_relative_qr_file() {
        let mut template_data = split_info("M001|L001|O001|10|V001|2026-08-07|B001");
        template_data.qr_code = Some(qr_code_data_uri("M001").expect("encode QR code"));
        let rendered = TEMPLATES
            .render(
                "template.html",
                &Context::from_serialize(&template_data).expect("serialize template data"),
            )
            .expect("agent-browser template should render");

        assert!(!rendered.contains("<img src=\"./qr.png\" height=\"150\" />"));
        assert!(rendered.contains("data:image/png;base64,"));
    }

    #[test]
    fn stores_temp_screenshot_next_to_the_executable_directory() {
        let temp_screenshot =
            TempScreenshot::new(std::path::Path::new("C:/code-github/qr_service"))
                .expect("create temp screenshot path");

        assert_eq!(
            temp_screenshot
                .path()
                .parent()
                .expect("screenshot path should have a parent"),
            std::path::Path::new("C:/code-github/qr_service/temp")
        );
    }
}
