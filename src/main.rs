pub mod err;
use actix_files::NamedFile;
use actix_web::{get, middleware, post, web, App, HttpResponse, HttpServer, Responder};
use err::CustomError;
use headless_chrome::{protocol::cdp::Page, Browser, Tab};
use image::{ImageFormat, Luma};
use lazy_static::lazy_static;
use qrcode::QrCode;
use resvg::{tiny_skia, usvg};
use serde::{Deserialize, Serialize};
use std::{env, fs::File, io::Cursor, sync::Arc};
use tera::{Context, Tera};
use tracing::info;
use tracing_subscriber::FmtSubscriber;
lazy_static! {
    pub static ref TEMPLATES: Tera = {
        let mut tera = match Tera::new("templates/**/*.html") {
            Ok(t) => t,
            Err(e) => {
                println!("Parsing error(s): {}", e);
                ::std::process::exit(1);
            }
        };
        tera.autoescape_on(vec![".html", ".sql"]);
        tera
    };
    pub static ref BROWSER: Browser = {
        let browser: Browser = Browser::default().unwrap();
        browser
    };
    pub static ref CTAB: Arc<Tab> = {
        let tab = BROWSER.new_tab().unwrap();
        tab
    };
}
#[get("/hello/{name}")]
async fn greet(name: web::Path<String>) -> Result<impl Responder, CustomError> {
    Ok(format!("Hello {name}!"))
}
#[get("/qr/{qr_code}")]
async fn get_qr_code(qr_code: web::Path<String>) -> Result<impl Responder, CustomError> {
    let code = QrCode::new(qr_code.as_bytes())?;
    let image = code.render::<Luma<u8>>().build();

    let mut buffer = Cursor::new(Vec::new());
    image.write_to(&mut buffer, ImageFormat::Png)?;
    Ok(HttpResponse::Ok()
        .content_type("image/png")
        .body(buffer.into_inner()))
}
#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    info!("1");
    info!("requests: {:#?}", labels);

    let code = QrCode::new(r"qr_code.as_bytes()")?;
    let image = code.render::<Luma<u8>>().build();
    image.save("./templates/qr.png")?;
    let mut result = File::create("./templates/result.html")?;
    TEMPLATES.render_to(
        "template.html",
        &Context::from_serialize(&labels)?,
        &mut result,
    )?;
    let tab = CTAB.clone();
    info!("2");
    let current_dir = env::current_dir()?;
    let file_path = current_dir.join("templates/result.html");
    let viewport = tab
        .navigate_to(&format!("file:///{}", file_path.display()))?
        .wait_for_element("table")?
        .get_box_model()?
        .margin_viewport();
    let jpeg_data = tab.capture_screenshot(
        Page::CaptureScreenshotFormatOption::Png,
        Some(75),
        Some(viewport),
        true,
    )?;
    info!("3");
    std::fs::write("screenshot.png", jpeg_data)?;
    Ok(NamedFile::open("screenshot.png")?)
}

#[actix_web::main] // or #[tokio::main]
async fn main() -> std::io::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
    HttpServer::new(|| {
        App::new()
            .service(greet)
            .service(get_qr_code)
            .service(create_label)
            .wrap(middleware::Logger::default())
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct LabelInfo {
    /// 类型：1：半成品，2：成品
    kind: i32,

    /// 订单号
    order_no: String,

    /// 客户名称
    customer_name: String,

    /// 型号
    product_model: String,

    /// 品名
    commodity: String,

    /// 二维码
    qr_code: String,

    is_return: bool,
}
