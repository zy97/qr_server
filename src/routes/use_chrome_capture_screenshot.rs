use actix_files::NamedFile;
use actix_web::{get, post, web, HttpResponse, Responder};
use barcoders::{generators::image::Image, sym::code128::Code128};
use base64::encode;
use headless_chrome::{
    browser::default_executable, protocol::cdp::Page, Browser, LaunchOptions, Tab,
};
use image::{ImageFormat, Luma};
use named_pipe::PipeOptions;
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use std::{
    env,
    ffi::OsStr,
    fs::File,
    io::{Cursor, Read},
    process::Command,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};
use tera::{Context, Tera};
use tracing::info;

use crate::err::CustomError;

static TEMPLATES: LazyLock<Tera> = LazyLock::new(|| {
    let mut tera =
        Tera::new("templates/chrome/template.html").expect("failed to load chrome template");
    tera.autoescape_on(vec![".html", ".sql"]);
    tera
});

static BROWSER: LazyLock<Browser> = LazyLock::new(|| {
    let launch_options = LaunchOptions::default_builder()
        .path(Some(default_executable().map_err(|e| e).unwrap()))
        .build()
        .unwrap();
    Browser::new(LaunchOptions {
        headless: true,
        args: vec![&OsStr::new("--disable-gpu")],
        ..launch_options
    })
    .unwrap()
});

static CTAB: LazyLock<Arc<Tab>> = LazyLock::new(|| BROWSER.new_tab().unwrap());

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/chrome")
            .service(get_qr_code)
            .service(get_barcode)
            .service(create_label)
            .service(print),
    );
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
    let barcode = Code128::new(format!("\u{00C0}{}", barcode)).unwrap();
    let png = Image::png(5);
    let bytes = png.generate(&barcode.encode()[..]).unwrap();

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
        image.save("templates/chrome/qr.png")?;

        let mut result = File::create("templates/chrome/result.html")?;
        TEMPLATES.render_to(
            "template.html",
            &Context::from_serialize(&infos)?,
            &mut result,
        )?;

        let file_path = env::current_dir()?.join("templates/chrome/result.html");
        info!("11111");
        let viewport = tab
            .navigate_to(&format!("file:///{}", file_path.display()))?
            .wait_for_element("table")?
            .get_box_model()?
            .margin_viewport();
        info!("22222");
        let image_data = tab.capture_screenshot(
            Page::CaptureScreenshotFormatOption::Png,
            Some(75),
            Some(viewport),
            true,
        )?;
        info!("33333");

        std::fs::write("chrome_result.png", image_data)?;
    }

    Ok(NamedFile::open("chrome_result.png")?)
}

#[post("/label1")]
async fn print(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    info!("111");
    for label in labels.0 {
        let code = QrCode::new(&label.qr_code)?;
        let mut infos = split_info(&label.qr_code, &label);
        let image = code.render::<Luma<u8>>().build();
        let mut buf = Vec::new();
        image.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)?;

        infos.base64 = Some(encode(&buf));
        info!("222");
        let json_string = serde_json::to_string(&infos).unwrap();
        let mut child = Command::new(r".\WpfApp2.exe")
            .args([json_string])
            .spawn()
            .map_err(|_| CustomError::PrinterNoFound)?;

        let timeout = Duration::from_secs(1);
        let start_time = Instant::now();
        loop {
            if start_time.elapsed() >= timeout {
                break;
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        info!("正常退出");
                        break;
                    } else {
                        info!("非正常退出");
                        receive_image_generate_success();
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }

        info!("333");
    }

    Ok(NamedFile::open("chrome_result.png")?)
}

fn receive_image_generate_success() -> bool {
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
                println!("收到数据: {received}");
                return true;
            }
            Ok(_) => break,
            Err(error) => {
                eprintln!("读取错误: {error}");
                break;
            }
        }
    }
    false
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
        descrpition: label.commodity.clone(),
        product_model: label.product_model.clone(),
    }
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct LabelInfo {
    kind: i32,
    order_no: String,
    customer_name: String,
    product_model: String,
    commodity: String,
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
