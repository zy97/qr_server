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
    time::{Duration, Instant},
};
use tera::{Context, Tera};
use tracing::info;

use crate::err::CustomError;
use crate::requests::dtos::create_lable_dto::LabelInfo;

const BROWSER_WINDOW_WIDTH: u32 = 800;
const BROWSER_WINDOW_HEIGHT: u32 = 500;

static TEMPLATES: LazyLock<Tera> = LazyLock::new(|| {
    let mut tera = Tera::new();
    tera.add_template_file("templates/chrome/template.html", Some("template.html"))
        .expect("failed to load chrome template");
    tera.autoescape_on(vec![".html", ".sql"]);
    tera
});

static BROWSER: LazyLock<Browser> = LazyLock::new(|| {
    let started = Instant::now();
    let launch_options = LaunchOptions::default_builder()
        .path(Some(default_executable().map_err(|e| e).unwrap()))
        .window_size(Some((BROWSER_WINDOW_WIDTH, BROWSER_WINDOW_HEIGHT)))
        .idle_browser_timeout(Duration::from_secs(86_400))
        .build()
        .unwrap();
    let browser = Browser::new(LaunchOptions {
        headless: true,
        args: vec![
            OsStr::new("--disable-gpu"),
            OsStr::new("--force-device-scale-factor=1"),
            OsStr::new("--disable-background-timer-throttling"),
            OsStr::new("--disable-renderer-backgrounding"),
            OsStr::new("--disable-backgrounding-occluded-windows"),
        ],
        ..launch_options
    })
    .unwrap();
    info!(
        elapsed_ms = started.elapsed().as_millis(),
        "chrome browser initialized"
    );
    browser
});

static CTAB: LazyLock<Mutex<Option<Arc<Tab>>>> = LazyLock::new(|| Mutex::new(None));

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(create_label);
}

#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    // 整个图片渲染时间大致在600-700ms附近跳动
    // 优化之后在200-500附近跳动
    let request_started = Instant::now();
    let labels = labels.into_inner();
    let label_count = labels.len();
    let mut result_image = None;

    for label in labels {
        let render_started = Instant::now();
        let mut infos = split_info(&label.qr_string, &label);
        infos.base64 = Some(qr_code_data_uri(&label.qr_string)?);
        infos.logo_base64 = Some(file_data_uri("templates/logo.png", "image/png")?);

        let rendered = TEMPLATES.render("template.html", &Context::from_serialize(&infos)?)?;
        info!(
            elapsed_ms = render_started.elapsed().as_millis(),
            "chrome template rendered"
        );

        let image_data = capture_label_screenshot(&rendered)?;
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

fn capture_label_screenshot(rendered: &str) -> Result<Vec<u8>, CustomError> {
    for attempt in 0..2 {
        let result = {
            let mut tab = CTAB
                .lock()
                .map_err(|error| CustomError::OtherLibraryError(error.to_string()))?;
            if tab.is_none() {
                let tab_started = Instant::now();
                *tab = Some(BROWSER.new_tab()?);
                info!(
                    elapsed_ms = tab_started.elapsed().as_millis(),
                    "chrome tab initialized"
                );
            }

            capture_label_screenshot_with_tab(
                tab.as_ref().expect("tab should be initialized"),
                rendered,
            )
        };

        match result {
            Ok(image_data) => return Ok(image_data),
            Err(error) if attempt == 0 && is_inactive_page_error(&error) => {
                let mut tab = CTAB
                    .lock()
                    .map_err(|error| CustomError::OtherLibraryError(error.to_string()))?;
                *tab = None;
                info!("chrome tab reset after inactive page");
            }
            Err(error) => return Err(error.into()),
        }
    }

    Err(CustomError::OtherLibraryError(
        "failed to capture label screenshot".to_string(),
    ))
}

fn capture_label_screenshot_with_tab(
    tab: &Arc<Tab>,
    rendered: &str,
) -> Result<Vec<u8>, anyhow::Error> {
    let navigate_started = Instant::now();
    let tab = tab.navigate_to(&html_data_url(rendered))?;
    info!(
        elapsed_ms = navigate_started.elapsed().as_millis(),
        "chrome page navigated"
    );

    let ready_started = Instant::now();
    let viewport = wait_for_label_paint(tab)?;
    info!(
        elapsed_ms = ready_started.elapsed().as_millis(),
        width = viewport.width,
        height = viewport.height,
        "chrome page ready"
    );

    let screenshot_started = Instant::now();
    let image_data = tab.capture_screenshot(
        Page::CaptureScreenshotFormatOption::Png,
        Some(75),
        Some(viewport),
        true,
    )?;
    info!(
        elapsed_ms = screenshot_started.elapsed().as_millis(),
        "chrome screenshot captured"
    );

    Ok(image_data)
}

fn wait_for_label_paint(tab: &Tab) -> Result<Page::Viewport, anyhow::Error> {
    let result = tab.evaluate(label_ready_script(), true)?;
    let value = result
        .value
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| anyhow::anyhow!("label ready script returned no viewport"))?;

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

fn parse_label_viewport(value: &str) -> Result<Page::Viewport, anyhow::Error> {
    let parts = value
        .split(',')
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()?;

    if parts.len() != 4 || parts[2] <= 0.0 || parts[3] <= 0.0 {
        anyhow::bail!("invalid label viewport: {value}");
    }

    Ok(Page::Viewport {
        x: parts[0].floor().max(0.0),
        y: parts[1].floor().max(0.0),
        width: parts[2].ceil(),
        height: parts[3].ceil(),
        scale: 1.0,
    })
}

fn is_inactive_page_error(error: &anyhow::Error) -> bool {
    error.to_string().contains("Not attached to an active page")
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
