pub mod err;
use actix_files::NamedFile;
use actix_web::{get, middleware, post, web, App, HttpServer, Responder};
use err::CustomError;
use printers::{common::base::job::PrinterJobOptions, get_printer_by_name};
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

        Command::new("powershell")
            .args([
                "-Command",
                "-NoProfile",
                "-WindowStyle",
                "Hidden",
                "Add-Type -AssemblyName System.Drawing;
             $pd = New-Object System.Drawing.Printing.PrintDocument;
             $pd.PrinterSettings.PrinterName = 'NPIFD3D7B (HP LaserJet MFP M233sdw)';
             $pd.add_PrintPage({
                 param($s, $e)
                 $e.Graphics.DrawString(
                     'Hello World',
                     (New-Object Drawing.Font('Arial', 20)),
                     [Drawing.Brushes]::Black,
                     100, 100
                 )
             });
             $pd.Print();",
            ])
            .spawn()
            .unwrap();
        // Command::new(r".\printer.exe")
        //     .args(&["main.png"])
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
    .bind(("127.0.0.1", 9095))?
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
