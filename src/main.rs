pub mod err;
use actix_files::NamedFile;
use actix_web::{get, middleware, post, web, App, HttpResponse, HttpServer, Responder};
use barcoders::{generators::image::Image, sym::code128::Code128};
use base64::encode;
use err::CustomError;
use headless_chrome::{
    browser::default_executable, protocol::cdp::Page, Browser, LaunchOptions, Tab,
};
use image::{ImageFormat, ImageReader, Luma};
use named_pipe::{PipeOptions, PipeServer};
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use std::{
    env,
    ffi::OsStr,
    fmt::Display,
    fs::File,
    io::{self, Cursor},
    os::windows::thread,
    process::Command,
    sync::{Arc, LazyLock},
    thread::sleep,
    time::{Duration, Instant},
};
use tera::{Context, Tera};
use tracing::info;
use tracing_subscriber::{field::display, fmt::Layer, layer::SubscriberExt, FmtSubscriber};

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
    // let browser: Browser = Browser::default().unwrap();
    let launch_options = LaunchOptions::default_builder()
        .path(Some(default_executable().map_err(|e| e).unwrap()))
        .build()
        .unwrap();
    let browser = Browser::new(LaunchOptions {
        headless: true,
        args: vec![&OsStr::new("--disable-gpu")],
        ..launch_options
    })
    .unwrap();
    browser
});
static CTAB: LazyLock<Arc<Tab>> = LazyLock::new(|| {
    let tab = BROWSER.new_tab().unwrap();
    tab
});

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
        let infos = split_info(&label.qr_code, &label);
        let image = code.render::<Luma<u8>>().build();
        image.save("./templates/qr.png")?;
        // let img = ImageReader::open("./templates/qr.png")?.decode()?;

        // // 将图像编码为 PNG 格式的字节数据
        // let mut img_bytes: Vec<u8> = Vec::new();
        // img.write_to(&mut Cursor::new(&mut img_bytes), image::ImageFormat::Png)?;

        // // 将字节数据转换为 Base64
        // let base64_string = general_purpose::STANDARD.encode(&img_bytes);

        let mut result = File::create("./templates/result.html")?;
        TEMPLATES.render_to(
            "template.html",
            &Context::from_serialize(&infos)?,
            &mut result,
        )?;

        let current_dir = env::current_dir()?;
        let file_path = current_dir.join("templates/result.html");
        info!("11111");
        let viewport = tab
            .navigate_to(&format!("file:///{}", file_path.display()))?
            .wait_for_element("table")?
            .get_box_model()?
            .margin_viewport();
        info!("22222");
        let jpeg_data = tab.capture_screenshot(
            Page::CaptureScreenshotFormatOption::Png,
            Some(75),
            Some(viewport),
            true,
        )?;
        info!("33333");

        std::fs::write("result.png", jpeg_data)?;
        // Command::new(r".\printer.exe")
        //     .args(&["result.png"])
        //     .output()
        //     .map_err(|_| CustomError::PrinterNoFound)?;
    }
    Ok(NamedFile::open("result.png")?)
}

#[post("/label1")]
async fn print(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    info!("111");
    for label in labels.0 {
        let code = QrCode::new(&label.qr_code)?;
        let mut infos = split_info(&label.qr_code, &label);
        let image = code.render::<Luma<u8>>().build();
        // image.save("./templates/qr.png")?;
        let mut buf = Vec::new();
        // image.write_to(&mut Cursor::new(&mut buf), image::ImageOutputFormat::Png)?;
        image.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)?;
        // // 将字节缓冲区转换为 base64 字符串
        let base64_string = encode(&buf);
        infos.base64 = Some(base64_string);
        info!("222");
        let json_string = serde_json::to_string(&infos).unwrap();
        let mut child = Command::new(r".\WpfApp2.exe")
            .args(&[json_string])
            .spawn()
            .map_err(|_| CustomError::PrinterNoFound)?;
        //
        let timeout = Duration::from_secs(1); // 10秒超时
        let start_time = Instant::now();

        // 不断检查是否超时
        loop {
            if start_time.elapsed() >= timeout {
                // 如果超时，终止子进程并返回错误
                break;
            }

            // 检查子进程是否结束
            match child.try_wait() {
                Ok(Some(n)) => {
                    if n.success() {
                        info!("正常退出");
                        break;
                    } else {
                        info!("非正常退出");
                        receive_image_generate_success();
                        break;
                    }
                }
                Ok(None) => {
                    // 进程仍在运行，继续等待
                }
                Err(_) => {
                    break;
                }
            }
        }

        info!("333");
    }
    Ok(NamedFile::open("result.png")?)
}
use std::io::{Read, Write};
pub fn receive_image_generate_success() -> bool {
    let pipe_name = r"\\.\pipe\SendResponse";
    let mut pipe = PipeOptions::new(pipe_name)
        .single()
        .unwrap()
        .wait()
        .unwrap();

    println!("命名管道已创建，等待客户端连接...");

    let mut buffer = [0u8; 1024];
    loop {
        match pipe.read(&mut buffer) {
            Ok(n) if n > 0 => {
                let received = String::from_utf8_lossy(&buffer[..n]);
                println!("收到数据: {}", received);
                return true;
            }
            Ok(_) => break,
            Err(e) => {
                eprintln!("读取错误: {}", e);
                break;
            }
        }
    }
    false
}
fn split_info(code: &str, lable: &LabelInfo) -> TemplateData {
    let infos = code.split('|').collect::<Vec<&str>>();
    TemplateData {
        material_no: infos[0].to_string(),
        lot_no: infos[1].to_string(),
        order_no: infos[2].to_string(),
        count: infos[3].to_string(),
        vender_code: infos[4].to_string(),
        date: infos[5].to_string(),
        box_no: infos[6].to_string(),
        customer_name: lable.customer_name.clone(),
        base64: None,
        descrpition: lable.commodity.clone(),
        product_model: lable.product_model.clone(),
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
            .service(get_qr_code)
            .service(create_label)
            .service(print)
            .service(get_barcode)
            .wrap(middleware::Logger::default())
    })
    .bind(("127.0.0.1", 9090))?
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
