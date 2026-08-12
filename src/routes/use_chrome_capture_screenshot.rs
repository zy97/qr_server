use actix_web::{post, web, HttpResponse, Responder};
use base64::{engine::general_purpose, Engine};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, Viewport};
use chromiumoxide::page::{Page, ScreenshotParams};
use futures::StreamExt;
use image::{DynamicImage, ImageFormat, Luma};
use qrcode::QrCode;
use serde::Serialize;
use std::{io::Cursor, time::Instant};
use tera::{Context, Tera};
use tokio::sync::{Mutex, OnceCell};
use tracing::info;

use crate::err::CustomError;
use crate::requests::dtos::create_lable_dto::LabelInfo;

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
    labels: web::Json<Vec<LabelInfo>>,
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
        let mut infos = split_info(&label.qr_string, &label);
        infos.qr_code = Some(qr_code_data_uri(&label.qr_string)?);

        let rendered = templates.render("template.html", &Context::from_serialize(&infos)?)?;
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
        part_no: label.part_no.clone(),
        material_name: label.material_name.clone(),
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

#[derive(Serialize, Debug)]
struct TemplateData {
    material_no: String,
    lot_no: String,
    order_no: String,
    count: String,
    vender_code: String,
    date: String,
    box_no: String,
    customer_name: String,
    part_no: String,
    material_name: String,
    qr_code: Option<String>,
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
        let label = LabelInfo {
            kind: 1,
            customer_name: "客户".to_string(),
            part_no: "P-001".to_string(),
            material_name: "物料".to_string(),
            qr_string: "M001|L001|O001|10|V001|2026-08-07|B001".to_string(),
            is_return: false,
        };
        let mut template_data = split_info(&label.qr_string, &label);
        template_data.qr_code = Some(qr_code_data_uri(&label.qr_string).expect("encode QR code"));
        let rendered = load_templates()
            .expect("load templates")
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
