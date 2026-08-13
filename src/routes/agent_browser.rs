use actix_web::{post, web, HttpResponse, Responder};
use base64::{engine::general_purpose, Engine};
use image::{DynamicImage, ImageFormat, Luma};
use qrcode::QrCode;
use std::{
    env, fs,
    io::Cursor,
    path::PathBuf,
    process::{self, Command},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tera::{Context, Tera};
use tracing::info;

use crate::err::CustomError;

/// 每次请求重新从磁盘加载模板：设计器保存 template.html 后无需重启即可生效
/// （解析约 150KB，毫秒级，相对浏览器截图耗时可忽略）
fn load_templates() -> Result<Tera, CustomError> {
    let mut tera = Tera::new();
    tera.add_template_file("templates/template.html", Some("template.html"))?;
    tera.autoescape_on(vec![".html", ".sql"]);
    Ok(tera)
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_label);
}

#[post("/label")]
async fn create_label(
    labels: web::Json<Vec<serde_json::Value>>,
) -> Result<impl Responder, CustomError> {
    // 整个图片渲染时间大致在200-300ms附近跳动
    let request_started = Instant::now();
    let labels = labels.into_inner();
    let label_count = labels.len();
    let current_dir = env::current_dir()?;
    let agent_browser_path = current_dir.join("agent-browser.exe");
    let agent_browser_dir = agent_browser_path
        .parent()
        .ok_or_else(|| CustomError::OtherLibraryError("invalid agent-browser path".to_string()))?;
    let mut result_image = None;

    let templates = load_templates()?;
    for label in labels {
        let render_started = Instant::now();
        let qr_string = label
            .get("qr_string")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| CustomError::OtherLibraryError("missing qr_string".to_string()))?;
        let mut context = build_template_context(qr_string, &label)?;
        context.insert("qr_code", &qr_code_data_uri(qr_string)?);

        let rendered = templates.render("template.html", &context)?;
        info!(
            elapsed_ms = render_started.elapsed().as_millis(),
            "agent-browser template rendered"
        );
        let output_path = TempScreenshot::new(agent_browser_dir)?;

        let browser_started = Instant::now();
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
        info!(
            elapsed_ms = browser_started.elapsed().as_millis(),
            "agent-browser command finished"
        );

        if !status.success() {
            return Err(CustomError::OtherLibraryError(format!(
                "agent-browser failed to render label with status {status}"
            )));
        }

        let read_started = Instant::now();
        let image_data = fs::read(output_path.path())?;
        info!(
            elapsed_ms = read_started.elapsed().as_millis(),
            "agent-browser screenshot read"
        );
        info!("agent-browser rendered {}", output_path.path().display());
        result_image = Some(image_data);
    }

    let result_image = result_image
        .ok_or_else(|| CustomError::OtherLibraryError("no label data provided".to_string()))?;
    info!(
        elapsed_ms = request_started.elapsed().as_millis(),
        label_count, "agent-browser label response finished"
    );

    Ok(HttpResponse::Ok()
        .content_type("image/png")
        .body(result_image))
}

fn command_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// 由请求 JSON 构建模板上下文：qr_string 按 | 分段 + 请求顶层字段（值转字符串）。
/// 字段名对应模板里的 {{ }} 占位符；新增字段只需请求方带上同名 key，无需改代码
fn build_template_context(
    qr_string: &str,
    label: &serde_json::Value,
) -> Result<Context, CustomError> {
    let parts: Vec<&str> = qr_string.split('|').collect();
    if parts.len() < 7 {
        return Err(CustomError::OtherLibraryError(format!(
            "qr_string 分段不足（需要 7 段）: {qr_string}"
        )));
    }
    let mut map = serde_json::Map::new();
    if let Some(obj) = label.as_object() {
        for (key, value) in obj {
            let text = match value {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                serde_json::Value::Bool(b) => Some(b.to_string()),
                _ => None,
            };
            if let Some(text) = text {
                map.insert(key.clone(), serde_json::Value::String(text));
            }
        }
    }
    // qr_string 分段是这些字段的权威来源
    for (key, value) in [
        ("material_no", parts[0]),
        ("lot_no", parts[1]),
        ("order_no", parts[2]),
        ("count", parts[3]),
        ("vender_code", parts[4]),
        ("date", parts[5]),
        ("box_no", parts[6]),
    ] {
        map.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }
    Ok(Context::from_serialize(&serde_json::Value::Object(map))?)
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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_QR: &str = "M001|L001|O001|10|V001|2026-08-07|B001";

    fn sample_label() -> serde_json::Value {
        serde_json::json!({
            "kind": 0,
            "customer_name": "测试客户",
            "part_no": "P-001",
            "material_name": "测试物料",
            "qr_string": SAMPLE_QR,
            "is_return": false
        })
    }

    #[test]
    fn renders_template_with_agent_browser_template_data() {
        let mut context =
            build_template_context(SAMPLE_QR, &sample_label()).expect("build context");
        context.insert("qr_code", &"data:image/png;base64,test");

        let rendered = load_templates()
            .expect("load templates")
            .render("template.html", &context)
            .expect("agent-browser template should render with master template data");

        // 与 main.typ 一致的动态字段都应渲染出来
        assert!(rendered.contains("P-001"));
        assert!(rendered.contains("测试物料"));
        assert!(rendered.contains("测试客户"));
        assert!(rendered.contains("M001"));
        assert!(rendered.contains("L001"));
        assert!(rendered.contains("O001"));
        assert!(rendered.contains("B001"));
    }

    #[test]
    fn renders_custom_fields_from_request() {
        let mut label = sample_label();
        label["custom_field"] = serde_json::json!("自定义值");
        let context =
            build_template_context(SAMPLE_QR, &label).expect("build context with custom field");
        let mut tera = Tera::default();
        tera.add_raw_template("t", "{{ custom_field }}|{{ count }}|{{ part_no }}")
            .expect("add raw template");

        assert_eq!(
            tera.render("t", &context).expect("render custom field"),
            "自定义值|10|P-001"
        );
    }

    #[test]
    fn rejects_qr_string_with_too_few_segments() {
        assert!(build_template_context("too-short", &sample_label()).is_err());
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
        let qr_code = qr_code_data_uri("M001").expect("encode QR code");
        let mut context =
            build_template_context(SAMPLE_QR, &sample_label()).expect("build context");
        context.insert("qr_code", &qr_code);
        let rendered = load_templates()
            .expect("load templates")
            .render("template.html", &context)
            .expect("agent-browser template should render");

        // 共用模板中 QR 图片必须渲染为传入的 data URI（相对路径形式只存在于 HTML 注释里）
        assert!(rendered.contains(&format!("src=\"{qr_code}\"")));
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
