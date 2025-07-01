pub mod err;
use actix_files::NamedFile;
use actix_web::{get, middleware, post, web, App, HttpServer, Responder};
use err::CustomError;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Write,
    process::{Command, Stdio},
};
use tracing::info;
use tracing_subscriber::{fmt::Layer, layer::SubscriberExt, FmtSubscriber};

#[get("/hello/{name}")]
async fn greet(name: web::Path<String>) -> Result<impl Responder, CustomError> {
    Ok(format!("Hello {name}!"))
}
#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    info!("0");
    for label in labels.0 {
        let json = serde_json::to_string_pretty(&label).expect("Failed to serialize");
        let mut file = File::create("data.json")?;
        file.write_all(json.as_bytes())?;
        info!("1");
        let status = Command::new("typst.exe")
            .arg("compile")
            .arg("main.typ")
            .arg("-f")
            .arg("png")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("Failed to run typst");

        if status.success() {
            println!("Compile finished successfully!");
        } else {
            println!("Compile failed.");
        }
        info!("2");
        // let code = QrCode::new(&label.qr_code)?;
        // let mut infos = split_info(&label.qr_code);
        // let image = code.render::<Luma<u8>>().build();
        // image.save("./templates/qr.png")?;
        // info!("00");
        // let img = ImageReader::open("./templates/qr.png")?.decode()?;

        // // 将图像编码为 PNG 格式的字节数据
        // let mut img_bytes: Vec<u8> = Vec::new();
        // img.write_to(&mut Cursor::new(&mut img_bytes), image::ImageFormat::Png)?;

        // // 将字节数据转换为 Base64
        // let base64_string = general_purpose::STANDARD.encode(&img_bytes);
        // info!("000");
        // infos.qr_code = Some(format!("data:image/png;base64,{}", base64_string));
        // let mut result = File::create("./templates/result.html")?;
        // TEMPLATES.render_to(
        //     "template.html",
        //     &Context::from_serialize(&infos)?,
        //     &mut result,
        // )?;

        // let current_dir = env::current_dir()?;
        // let file_path = current_dir.join("templates/result.html");
        // info!("0000");
        // let viewport = tab
        //     .navigate_to(&format!("file:///{}", file_path.display()))?
        //     .wait_for_element("#app")?;

        // .get_box_model()?
        // .margin_viewport();
        // let jpeg_data = tab.capture_screenshot(
        //     Page::CaptureScreenshotFormatOption::Png,
        //     Some(75),
        //     Some(viewport),
        //     true,
        // )?;

        // let result = viewport.call_js_fn(
        //     r#"
        //         function getIdTwice () {
        //           return html2canvas(document.getElementById('app')).then(function(canvas) {
        //             document.body.appendChild(canvas)
        //             console.log(canvas,231);
        //             let sdf = canvas.toDataURL();
        //             console.log("sdsdfsd",sdf);
        //             return sdf;
        //             });
        //         }

        // "#,
        //     vec![],
        //     true,
        // )?;

        // match result.value {
        //     Some(returned_string) => {
        //         // dbg!(returned_string);
        //         let sdf: &str = returned_string.as_str().unwrap();
        //         let sdf = sdf.trim_start_matches("data:image/png;base64,");
        //         let decoded_bytes = decode(sdf).map_err(|e| {
        //             io::Error::new(
        //                 io::ErrorKind::InvalidData,
        //                 format!("Failed to decode base64: {}", e),
        //             )
        //         })?;
        //         let mut file = File::create("result.png")?;

        //         // 写入解码后的字节数据到文件
        //         file.write_all(&decoded_bytes)?;
        //     }
        //     _ => unreachable!(),
        // };
        info!("3");

        // std::fs::write("result.png", jpeg_data)?;
        // Command::new(r".\printer.exe")
        //     .args(&["result.png"])
        //     .output()
        //     .map_err(|_| CustomError::PrinterNoFound)?;
    }
    Ok(NamedFile::open("main.png")?)
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
            .service(create_label)
            .wrap(middleware::Logger::default())
    })
    .bind(("127.0.0.1", 9090))?
    .run()
    .await
}

#[derive(Deserialize, Serialize, Debug)]
struct LabelInfo {
    /// 类型：1：半成品，2：成品
    kind: i32,
    /// 客户名称
    customer_name: String,
    /// 型号
    part_no: String,
    /// 品名
    material_name: String,
    /// 二维码
    qr_string: String,
    is_return: bool,
}
