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

/// 每次请求从数据库读取默认模板的渲染 HTML 构建 Tera：
/// 设计器保存/切换默认模板后立即生效（解析毫秒级，相对浏览器截图耗时可忽略）
fn load_templates(selector: Option<&str>) -> Result<(Tera, String), CustomError> {
    let html = crate::template_store::with_db(|conn| {
        crate::template_store::render_html_for(conn, selector)
    })?;
    let tera = templates_from_html(&html)?;
    Ok((tera, html))
}

fn templates_from_html(html: &str) -> Result<Tera, CustomError> {
    let mut tera = Tera::new();
    tera.add_raw_template("template.html", html)?;
    tera.autoescape_on(vec![".html", ".sql"]);
    Ok(tera)
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_label);
}

/// /label 查询参数：template 指定渲染模板（id 或名称），缺省用默认模板；print=false 跳过打印（模板设计器预览用）
#[derive(serde::Deserialize)]
pub struct LabelQuery {
    template: Option<String>,
    print: Option<bool>,
}

/// 与 chrome 渲染路径一致的打印开关语义：配置开启且请求未显式 print=false 才打印
fn should_print(config_enabled: bool, query: &LabelQuery) -> bool {
    config_enabled && query.print != Some(false)
}

#[post("/label")]
async fn create_label(
    labels: web::Json<Vec<serde_json::Value>>,
    query: web::Query<LabelQuery>,
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

    let (templates, template_html) = load_templates(query.template.as_deref())?;
    for label in labels {
        let render_started = Instant::now();
        let context = build_template_context(&label, &template_html)?;

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
        if should_print(crate::config::CONFIG.print.enabled, &query) {
            crate::print::print_label_png(&image_data).await?;
        }
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
    label: &serde_json::Value,
    template_html: &str,
) -> Result<Context, CustomError> {
    // qr_string 非必填：缺失时分段字段留空、qr_code 置空（新模板/预览可能没有）；
    // 提供了但不足 7 段才视为数据错误
    let qr_string = label
        .get("qr_string")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let mut map = serde_json::Map::new();
    if let Some(obj) = label.as_object() {
        for (key, value) in obj {
            // 字符串/数字/布尔保持原生类型进入上下文（模板里可做 == true、if is_return 等判断）
            if value.is_string() || value.is_number() || value.is_boolean() {
                map.insert(key.clone(), value.clone());
            }
        }
    }
    if !qr_string.is_empty() {
        let parts: Vec<&str> = qr_string.split('|').collect();
        if parts.len() < 7 {
            return Err(CustomError::OtherLibraryError(format!(
                "qr_string 分段不足（需要 7 段）: {qr_string}"
            )));
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
    }
    apply_split_variables(&mut map, template_html);
    apply_image_variables(&mut map, template_html);
    fill_missing_variables(&mut map, template_html);

    let mut context = Context::from_serialize(&serde_json::Value::Object(map))?;
    if qr_string.is_empty() {
        context.insert("qr_code", &"");
    } else {
        context.insert("qr_code", &qr_code_data_uri(qr_string)?);
    }
    Ok(context)
}

/// 模板里形如 {{ _x_<字段>_<分隔符hex>_<下标> }} 的拆分变量：
/// 取上下文中对应字段的值，按分隔符切分后取下标（字段缺失或越界给空串）
fn apply_split_variables(
    map: &mut serde_json::Map<String, serde_json::Value>,
    template_html: &str,
) {
    let mut rest = template_html;
    while let Some(start) = rest.find("{{ _x_") {
        let after = &rest[start + 6..];
        let Some(end) = after.find("}}") else { break };
        let key = after[..end].trim();
        rest = &after[end + 2..];
        // 从尾部解析：<字段>_<分隔符hex>_<下标>（字段名本身可能含下划线）
        let body = key; // find 的 "{{ _x_" 已含 _x_ 前缀，此处 key 即字段起始
        let Some(i1) = body.rfind('_') else { continue };
        let (head, index_str) = (&body[..i1], &body[i1 + 1..]);
        let Some(i2) = head.rfind('_') else { continue };
        let (field, sep_hex) = (&head[..i2], &head[i2 + 1..]);
        let Ok(index) = index_str.parse::<usize>() else {
            continue;
        };
        let Some(sep) = decode_hex(sep_hex).and_then(|b| String::from_utf8(b).ok()) else {
            continue;
        };
        if sep.is_empty() {
            continue;
        }
        let value = map.get(field).and_then(|v| v.as_str()).unwrap_or_default();
        let part = value.split(&sep).nth(index).unwrap_or_default().to_string();
        if part.is_empty() && !value.is_empty() {
            tracing::warn!(field, index, "split variable out of range");
        }
        map.insert(format!("_x_{body}"), serde_json::Value::String(part));
    }
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.is_empty() || hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}
/// 模板里 {{ _qr_<字段> }} / {{ _bar_<字段> }} 变量（由 {{ 字段@qr }} / {{ 字段@条形码 }} 编译而来）：
/// 把对应字段值生成为二维码/一维码图片，值为完整 <img> 标签（模板里以 | safe 输出）
fn apply_image_variables(
    map: &mut serde_json::Map<String, serde_json::Value>,
    template_html: &str,
) {
    for (marker, is_qr) in [("{{ _qr_", true), ("{{ _bar_", false)] {
        let mut rest = template_html;
        while let Some(start) = rest.find(marker) {
            let after = &rest[start + marker.len()..];
            let Some(end) = after.find("}}") else { break };
            let var = after[..end].trim();
            rest = &after[end + 2..];
            // var 形如 _qr_qr_string 或 _qr_qr_string | safe
            let field = var.split('|').next().unwrap_or("").trim();
            let name = if is_qr {
                format!("_qr_{field}")
            } else {
                format!("_bar_{field}")
            };
            let value = map.get(field).and_then(|v| v.as_str()).unwrap_or_default();
            let img = if value.is_empty() {
                String::new()
            } else {
                let uri = if is_qr {
                    qr_code_data_uri(value)
                } else {
                    barcode_data_uri(value)
                };
                match uri {
                    Ok(uri) => format!(
                        r#"<img src="{uri}" style="width:100%;height:100%;object-fit:contain">"#
                    ),
                    Err(_) => String::new(),
                }
            };
            map.insert(name.to_string(), serde_json::Value::String(img));
        }
    }
}

/// Code128 一维码 → PNG data URI
fn barcode_data_uri(content: &str) -> Result<String, CustomError> {
    // barcoders 的 Code128 要求数据以字符集标记开头：À=A Ɓ=B(可打印 ASCII) Ć=C(纯数字)
    let data = match content.chars().next() {
        Some('À') | Some('Ɓ') | Some('Ć') => content.to_string(),
        _ => format!("Ɓ{content}"),
    };
    let barcode = barcoders::sym::code128::Code128::new(data)
        .map_err(|e| CustomError::OtherLibraryError(format!("条形码内容非法: {e}")))?;
    let encoded = barcode.encode();
    let png = barcoders::generators::image::Image::png(60);
    let bytes = png
        .generate(&encoded[..])
        .map_err(|e| CustomError::OtherLibraryError(format!("条形码生成失败: {e}")))?;
    Ok(format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(bytes)
    ))
}

/// 模板里 {{ 标识符 }} 形式的变量在上下文中缺失时补空串，
/// 避免单个字段缺失导致整单渲染失败（拆分变量由 apply_split_variables 处理，此处跳过）
fn fill_missing_variables(
    map: &mut serde_json::Map<String, serde_json::Value>,
    template_html: &str,
) {
    let mut rest = template_html;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let key = after[..end].trim();
        rest = &after[end + 2..];
        let is_ident = key
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if is_ident && !key.starts_with("_x_") {
            map.entry(key.to_string())
                .or_insert_with(|| serde_json::Value::String(String::new()));
        }
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
        let context = build_template_context(
            &sample_label(),
            &std::fs::read_to_string("templates/template.html").expect("read template"),
        )
        .expect("build context");

        let rendered = templates_from_html(
            &std::fs::read_to_string("templates/template.html").expect("read template"),
        )
        .expect("build templates")
        .render("template.html", &context)
        .expect("agent-browser template should render with template data");

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
        let context = build_template_context(&label, "").expect("build context with custom field");
        let mut tera = Tera::default();
        tera.add_raw_template("t", "{{ custom_field }}|{{ count }}|{{ part_no }}")
            .expect("add raw template");

        assert_eq!(
            tera.render("t", &context).expect("render custom field"),
            "自定义值|10|P-001"
        );
    }

    #[test]
    fn renders_ternary_expression_with_bool_field() {
        // Tera 三元语法为 Python 风格："值1" if 条件 else "值2"
        let mut label = sample_label();
        label["is_return"] = serde_json::json!(true);
        let context = build_template_context(&label, "").expect("build context");
        let mut tera = Tera::default();
        tera.add_raw_template("t", r#"{{ "回" if is_return else "" }}"#)
            .expect("add raw template");

        assert_eq!(tera.render("t", &context).expect("render"), "回");

        label["is_return"] = serde_json::json!(false);
        let context = build_template_context(&label, "").expect("build context");
        assert_eq!(tera.render("t", &context).expect("render"), "");
    }

    #[test]
    fn missing_template_variable_renders_as_empty() {
        // 请求数据里没有的字段渲染为空串，而不是整单报错
        let context = build_template_context(&sample_label(), "{{ part_no }}|{{ not_in_request }}")
            .expect("build context");
        let mut tera = Tera::default();
        tera.add_raw_template("t", "{{ part_no }}|{{ not_in_request }}")
            .expect("add raw template");

        assert_eq!(tera.render("t", &context).expect("render"), "P-001|");
    }

    #[test]
    fn renders_qr_and_barcode_variables() {
        // {{ _qr_order_no }} / {{ _bar_order_no }}：字段值生成二维码/一维码图片
        let context = build_template_context(
            &sample_label(),
            "{{ _qr_order_no | safe }}#{{ _bar_order_no | safe }}",
        )
        .expect("build context");
        let mut tera = Tera::default();
        tera.add_raw_template("t", "{{ _qr_order_no | safe }}#{{ _bar_order_no | safe }}")
            .expect("add raw template");
        let rendered = tera.render("t", &context).expect("render");
        let parts: Vec<&str> = rendered.split('#').collect();
        assert!(parts[0].contains("data:image/png;base64,"));
        assert!(parts[1].contains("data:image/png;base64,"));
    }

    #[test]
    fn barcode_data_uri_generates_png() {
        let uri = barcode_data_uri("123456").expect("barcode should encode");
        let bytes = general_purpose::STANDARD
            .decode(
                uri.strip_prefix("data:image/png;base64,")
                    .expect("data uri"),
            )
            .expect("decode");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn rejects_qr_string_with_too_few_segments() {
        let mut label = sample_label();
        label["qr_string"] = serde_json::json!("too-short");
        assert!(build_template_context(&label, "").is_err());
    }

    #[test]
    fn renders_split_variable_with_custom_separator() {
        // {{ _x_qr_string_7c_6 }}：qr_string 按 | 切分取第 6 段；{{ _x_date_2d_1 }}：date 按 - 切分取第 1 段
        let context = build_template_context(
            &sample_label(),
            "{{ _x_qr_string_7c_6 }}|{{ _x_date_2d_1 }}",
        )
        .expect("build context");
        let mut tera = Tera::default();
        tera.add_raw_template("t", "{{ _x_qr_string_7c_6 }}|{{ _x_date_2d_1 }}")
            .expect("add raw template");

        assert_eq!(tera.render("t", &context).expect("render"), "B001|08");
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
        let context = build_template_context(
            &sample_label(),
            &std::fs::read_to_string("templates/template.html").expect("read template"),
        )
        .expect("build context");
        let rendered = templates_from_html(
            &std::fs::read_to_string("templates/template.html").expect("read template"),
        )
        .expect("build templates")
        .render("template.html", &context)
        .expect("agent-browser template should render");

        // 模板中 QR 图片必须渲染为 qr_code data URI
        assert!(rendered.contains("src=\"data:image/png;base64,"));
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
