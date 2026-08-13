use actix_web::{post, web, HttpResponse, Responder};
use base64::{engine::general_purpose, Engine};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, Viewport};
use chromiumoxide::page::{Page, ScreenshotParams};
use futures::StreamExt;
use image::{DynamicImage, ImageFormat, Luma};
use qrcode::QrCode;
use std::{io::Cursor, time::Instant};
use tera::{Context, Tera};
use tokio::sync::{Mutex, OnceCell};
use tracing::info;

use crate::err::CustomError;

const BROWSER_WINDOW_WIDTH: u32 = 800;
const BROWSER_WINDOW_HEIGHT: u32 = 500;

/// 每次请求重新从磁盘加载模板：设计器保存 template.html 后无需重启即可生效
/// （解析约 150KB，毫秒级，相对浏览器截图耗时可忽略）
fn load_templates() -> Result<Tera, CustomError> {
    let mut tera = Tera::new();
    tera.add_template_file("templates/template.html", Some("template.html"))?;
    tera.autoescape_on(vec![".html", ".sql"]);
    Ok(tera)
}

struct ChromeState {
    browser: Browser,
    page: Mutex<Option<Page>>,
}

static CHROME: OnceCell<ChromeState> = OnceCell::const_new();

// 首次请求时惰性启动浏览器；chromiumoxide 只探测本机已安装的
// Chrome/Chromium/Edge，不会自动下载浏览器
async fn chrome_state() -> Result<&'static ChromeState, CustomError> {
    CHROME
        .get_or_try_init(|| async {
            let started = Instant::now();
            // 按进程使用独立 user-data-dir，避免残留浏览器进程持有单例锁导致启动即退出
            let user_data_dir =
                std::env::temp_dir().join(format!("qr-service-chrome-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&user_data_dir);
            let config = BrowserConfig::builder()
                .window_size(BROWSER_WINDOW_WIDTH, BROWSER_WINDOW_HEIGHT)
                .user_data_dir(&user_data_dir)
                .args([
                    "--disable-gpu",
                    "--force-device-scale-factor=1",
                    "--disable-background-timer-throttling",
                    "--disable-renderer-backgrounding",
                    "--disable-backgrounding-occluded-windows",
                ])
                .build()
                .map_err(anyhow::Error::msg)?;
            let (browser, mut handler) = Browser::launch(config).await?;
            // handler 驱动 websocket，必须持续轮询
            actix_web::rt::spawn(async move {
                while let Some(event) = handler.next().await {
                    if event.is_err() {
                        break;
                    }
                }
            });
            info!(
                elapsed_ms = started.elapsed().as_millis(),
                "chrome browser initialized"
            );
            Ok::<ChromeState, anyhow::Error>(ChromeState {
                browser,
                page: Mutex::new(None),
            })
        })
        .await
        .map_err(CustomError::from)
}

/// /label 查询参数：?print=false 跳过打印（模板设计器预览用）
#[derive(serde::Deserialize)]
pub struct LabelQuery {
    print: Option<bool>,
}

fn should_print(config_enabled: bool, query: &LabelQuery) -> bool {
    config_enabled && query.print != Some(false)
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_label);
}

#[post("/label")]
async fn create_label(
    labels: web::Json<Vec<serde_json::Value>>,
    query: web::Query<LabelQuery>,
) -> Result<impl Responder, CustomError> {
    // 使用chromiumoxide重构耗时在200ms附近跳动
    let request_started = Instant::now();
    let labels = labels.into_inner();
    let label_count = labels.len();
    let mut result_image = None;

    let templates = load_templates()?;
    for label in labels {
        let render_started = Instant::now();
        let template_html = std::fs::read_to_string("templates/template.html")?;
        let context = build_template_context(&label, &template_html)?;

        let rendered = templates.render("template.html", &context)?;
        info!(
            elapsed_ms = render_started.elapsed().as_millis(),
            "chrome template rendered"
        );

        let image_data = capture_label_screenshot(&rendered).await?;
        if should_print(crate::config::CONFIG.print.enabled, &query) {
            crate::print::print_label_png(&image_data)?;
        }
        result_image = Some(image_data);
    }

    let result_image = result_image
        .ok_or_else(|| CustomError::OtherLibraryError("no label data provided".to_string()))?;
    info!(
        elapsed_ms = request_started.elapsed().as_millis(),
        label_count, "chrome label response finished"
    );

    Ok(HttpResponse::Ok()
        .content_type("image/png")
        .body(result_image))
}

async fn capture_label_screenshot(rendered: &str) -> Result<Vec<u8>, CustomError> {
    let state = chrome_state().await?;

    for attempt in 0..2 {
        let result = {
            let mut page = state.page.lock().await;
            if page.is_none() {
                let page_started = Instant::now();
                *page = Some(
                    state
                        .browser
                        .new_page("about:blank")
                        .await
                        .map_err(anyhow::Error::new)?,
                );
                info!(
                    elapsed_ms = page_started.elapsed().as_millis(),
                    "chrome page initialized"
                );
            }

            capture_label_screenshot_with_page(
                page.as_ref().expect("page should be initialized"),
                rendered,
            )
            .await
        };

        match result {
            Ok(image_data) => return Ok(image_data),
            Err(error) if attempt == 0 => {
                let mut page = state.page.lock().await;
                *page = None;
                info!(%error, "chrome page reset after capture failure");
            }
            Err(error) => return Err(error.into()),
        }
    }

    Err(CustomError::OtherLibraryError(
        "failed to capture label screenshot".to_string(),
    ))
}

async fn capture_label_screenshot_with_page(
    page: &Page,
    rendered: &str,
) -> Result<Vec<u8>, anyhow::Error> {
    let navigate_started = Instant::now();
    page.goto(html_data_url(rendered)).await?;
    info!(
        elapsed_ms = navigate_started.elapsed().as_millis(),
        "chrome page navigated"
    );

    let ready_started = Instant::now();
    let viewport = wait_for_label_paint(page).await?;
    info!(
        elapsed_ms = ready_started.elapsed().as_millis(),
        width = viewport.width,
        height = viewport.height,
        "chrome page ready"
    );

    let screenshot_started = Instant::now();
    let image_data = page
        .screenshot(
            ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .quality(75)
                .clip(viewport)
                .from_surface(true)
                .capture_beyond_viewport(true)
                .build(),
        )
        .await?;
    info!(
        elapsed_ms = screenshot_started.elapsed().as_millis(),
        "chrome screenshot captured"
    );

    Ok(image_data)
}

async fn wait_for_label_paint(page: &Page) -> Result<Viewport, anyhow::Error> {
    let value: String = page
        .evaluate(label_ready_script())
        .await?
        .into_value()
        .map_err(|error| anyhow::anyhow!("label ready script returned no viewport: {error}"))?;

    parse_label_viewport(&value)
}

fn label_ready_script() -> &'static str {
    r##"
        new Promise((resolve) => {
            let done = false;
            const fallback = setTimeout(() => finish(), 500);

            const ready = () => {
                const app = document.querySelector("#app") || document.querySelector("table");
                if (!app) {
                    return false;
                }

                const rect = app.getBoundingClientRect();
                const imagesReady = Array.from(document.images).every((img) => img.complete);
                return document.readyState === "complete"
                    && imagesReady
                    && rect.width > 0
                    && rect.height > 0;
            };

            const finish = () => {
                if (done) {
                    return;
                }
                done = true;
                clearTimeout(fallback);
                requestAnimationFrame(() => {
                    const app = document.querySelector("#app") || document.querySelector("table");
                    if (!app) {
                        resolve("0,0,1,1");
                        return;
                    }
                    const rect = app.getBoundingClientRect();
                    resolve(`${rect.left},${rect.top},${rect.width},${rect.height}`);
                });
            };

            if (ready()) {
                finish();
                return;
            }

            const timer = setInterval(() => {
                if (ready()) {
                    clearInterval(timer);
                    finish();
                }
            }, 10);
        })
    "##
}

fn parse_label_viewport(value: &str) -> Result<Viewport, anyhow::Error> {
    let parts = value
        .split(',')
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()?;

    if parts.len() != 4 || parts[2] <= 0.0 || parts[3] <= 0.0 {
        anyhow::bail!("invalid label viewport: {value}");
    }

    Ok(Viewport {
        x: parts[0].floor().max(0.0),
        y: parts[1].floor().max(0.0),
        width: parts[2].ceil(),
        height: parts[3].ceil(),
        scale: 1.0,
    })
}

/// 由请求 JSON 构建模板上下文：qr_string 按 | 分段 + 请求顶层字段（值转字符串）。
/// 字段名对应模板里的 {{ }} 占位符；新增字段只需请求方带上同名 key，无需改代码
fn build_template_context(
    label: &serde_json::Value,
    template_html: &str,
) -> Result<Context, CustomError> {
    let qr_string = label
        .get("qr_string")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| CustomError::OtherLibraryError("missing qr_string".to_string()))?;
    let parts: Vec<&str> = qr_string.split('|').collect();
    if parts.len() < 7 {
        return Err(CustomError::OtherLibraryError(format!(
            "qr_string 分段不足（需要 7 段）: {qr_string}"
        )));
    }
    let mut map = serde_json::Map::new();
    if let Some(obj) = label.as_object() {
        for (key, value) in obj {
            // 字符串/数字/布尔保持原生类型进入上下文（模板里可做 == true、if is_return 等判断）
            if value.is_string() || value.is_number() || value.is_boolean() {
                map.insert(key.clone(), value.clone());
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
    apply_split_variables(&mut map, template_html);
    fill_missing_variables(&mut map, template_html);

    let mut context = Context::from_serialize(&serde_json::Value::Object(map))?;
    context.insert("qr_code", &qr_code_data_uri(qr_string)?);
    Ok(context)
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

fn html_data_url(html: &str) -> String {
    format!(
        "data:text/html;base64,{}",
        general_purpose::STANDARD.encode(html)
    )
}

#[cfg(test)]
mod tests {

    #[test]
    fn print_query_flag_overrides_config() {
        let default = LabelQuery { print: None };
        let off = LabelQuery { print: Some(false) };
        let on = LabelQuery { print: Some(true) };
        assert!(should_print(true, &default));
        assert!(!should_print(false, &default));
        assert!(!should_print(true, &off));
        assert!(should_print(true, &on));
    }
    use super::*;

    const SAMPLE_QR: &str = "M001|L001|O001|10|V001|2026-08-07|B001";

    fn sample_label() -> serde_json::Value {
        serde_json::json!({
            "kind": 0,
            "customer_name": "客户",
            "part_no": "P-001",
            "material_name": "物料",
            "qr_string": SAMPLE_QR,
            "is_return": false
        })
    }

    #[test]
    fn renders_template_with_chrome_template_data() {
        let context = build_template_context(
            &sample_label(),
            &std::fs::read_to_string("templates/template.html").expect("read template"),
        )
        .expect("build context");

        let rendered = load_templates()
            .expect("load templates")
            .render("template.html", &context)
            .expect("chrome template should render with chrome template data");

        // 与 main.typ 一致的动态字段都应渲染出来
        assert!(rendered.contains("P-001"));
        assert!(rendered.contains("物料"));
        assert!(rendered.contains("客户"));
        assert!(rendered.contains("M001"));
        assert!(rendered.contains("L001"));
        assert!(rendered.contains("O001"));
        assert!(rendered.contains("B001"));
    }

    #[test]
    fn renders_template_without_relative_image_files() {
        let context = build_template_context(
            &sample_label(),
            &std::fs::read_to_string("templates/template.html").expect("read template"),
        )
        .expect("build context");
        let rendered = load_templates()
            .expect("load templates")
            .render("template.html", &context)
            .expect("chrome template should render");

        assert!(!rendered.contains("../logo.png"));
        assert!(!rendered.contains("./qr.png"));
        assert!(rendered.contains("data:image/png;base64,"));
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

    #[test]
    fn parses_label_viewport_from_rendered_table_rect() {
        let viewport = parse_label_viewport("0,0,685.36,349.2").expect("parse viewport");

        assert_eq!(viewport.x, 0.0);
        assert_eq!(viewport.y, 0.0);
        assert_eq!(viewport.width, 686.0);
        assert_eq!(viewport.height, 350.0);
        assert_eq!(viewport.scale, 1.0);
    }

    #[test]
    fn waits_for_document_images_and_label_layout_before_screenshot() {
        let script = label_ready_script();

        assert!(script.contains("document.readyState"));
        assert!(script.contains("document.images"));
        assert!(script.contains("getBoundingClientRect"));
        assert!(script.contains("requestAnimationFrame"));
    }
}
