pub mod err;
use actix_files::NamedFile;
use actix_web::{get, middleware, post, web, App, HttpResponse, HttpServer, Responder};
use barcoders::{generators::image::Image, sym::code128::Code128};
use err::CustomError;
use headless_chrome::{protocol::cdp::Page, Browser, Tab};
use image::{ImageFormat, Luma};
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::File,
    io::{self, Cursor},
    process::Command,
    sync::{Arc, LazyLock},
};
use tera::{Context, Tera};
use tracing::info;
use tracing_subscriber::{fmt::Layer, layer::SubscriberExt, FmtSubscriber};
// lazy_static! {
//     // pub static ref TEMPLATES: Tera = {
//     //     let mut tera = match Tera::new("templates/**/*.html") {
//     //         Ok(t) => t,
//     //         Err(e) => {
//     //             println!("Parsing error(s): {}", e);
//     //             ::std::process::exit(1);
//     //         }
//     //     };
//     //     tera.autoescape_on(vec![".html", ".sql"]);
//     //     tera
//     // };
//     pub static ref BROWSER: Browser = {
//         let browser: Browser = Browser::default().unwrap();
//         browser
//     };
//     pub static ref CTAB: Arc<Tab> = {
//         let tab = BROWSER.new_tab().unwrap();
//         tab
//     };
// }
static TEMPLATES: LazyLock<Tera> = LazyLock::new(|| {
    let mut tera = match Tera::new("templates/**/*.html") {
        Ok(t) => t,
        Err(e) => {
            println!("Parsing error(s): {}", e);
            ::std::process::exit(1);
        }
    };
    tera.autoescape_on(vec![".html", ".sql"]);
    tera
});
static BROWSER: LazyLock<Browser> = LazyLock::new(|| {
    let browser: Browser = Browser::default().unwrap();
    browser
});
static CTAB: LazyLock<Arc<Tab>> = LazyLock::new(|| {
    let tab = BROWSER.new_tab().unwrap();
    tab
});

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
#[get("/barcode/{barcode}")]
async fn get_barcode(barcode: web::Path<String>) -> Result<impl Responder, CustomError> {
    //code128 生成不了O202400043条形码数据需要加上前缀，查考https://github.com/buntine/barcoders/blob/master/src/sym/code128.rs最后的测试
    // 但code39可以直接生成
    let barcode = Code128::new(format!("\u{00C0}{}", barcode)).unwrap();
    let png = Image::png(5); // You must specify the height in pixels.
    let encoded = barcode.encode();
    // Image generators return a Result<Vec<u8>, barcoders::error::Error) of encoded bytes.
    let bytes = png.generate(&encoded[..]).unwrap();

    Ok(HttpResponse::Ok()
        .content_type("image/png")
        .body(Cursor::new(bytes).into_inner()))
}
#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    let tab = CTAB.clone();
    for label in labels.0 {
        let code = QrCode::new(&label.qr_code)?;
        let infos = split_info(&label.qr_code);
        let image = code.render::<Luma<u8>>().build();
        image.save("./templates/qr.png")?;
        let mut result = File::create("./templates/result.html")?;
        TEMPLATES.render_to(
            "template.html",
            &Context::from_serialize(&infos)?,
            &mut result,
        )?;

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
        std::fs::write("result.png", jpeg_data)?;
        // Command::new(r".\printer.exe")
        //     .args(&["result.png"])
        //     .output()
        //     .map_err(|_| CustomError::PrinterNoFound)?;
    }
    Ok(NamedFile::open("result.png")?)
}

// #[post("/label")]
// async fn create_label11(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
//     // create a `Browser` that spawns a `chromium` process running with UI (`with_head()`, headless is default)
//     // and the handler that drives the websocket etc.
//     let (mut browser, mut handler) =
//         Browser::launch(BrowserConfig::builder().with_head().build().unwrap())
//             .await
//             .unwrap();

//     // spawn a new task that continuously polls the handler
//     let handle = async_std::task::spawn(async move {
//         while let Some(h) = handler.next().await {
//             if h.is_err() {
//                 break;
//             }
//         }
//     });

//     // create a new browser page and navigate to the url
//     let page = browser.new_page("https://en.wikipedia.org").await.unwrap();
//     Ok(format!("Hello !"))
// }

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
    }
}
#[actix_web::main] // or #[tokio::main]
async fn main() -> std::io::Result<()> {
    let file_appender = tracing_appender::rolling::daily("logs", "app.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // 创建一个文件输出层
    let file_layer = Layer::new().with_writer(non_blocking); // 输出到文件

    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .finish()
        .with(file_layer);
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    HttpServer::new(|| {
        App::new()
            .service(greet)
            .service(get_qr_code)
            .service(create_label)
            .service(get_barcode)
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
#[derive(Serialize)]
struct TemplateData {
    material_no: String,
    lot_no: String,
    order_no: String,
    count: String,
    vender_code: String,
    date: String,
    box_no: String,
}
