use actix_files::NamedFile;
use actix_web::{get, post, web, Responder};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Write,
    process::{Command, Stdio},
};
use tracing::info;

use crate::err::CustomError;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(greet).service(create_label);
}

#[get("/hello/{name}")]
async fn greet(name: web::Path<String>) -> Result<impl Responder, CustomError> {
    Ok(format!("Hello {name}!"))
}

#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    info!("0");
    // 整个图片渲染时间大致在300-400ms附近跳动
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

        // Command::new("powershell")
        //     .args([
        //         "-Command",
        //         "-NoProfile",
        //         "-WindowStyle",
        //         "Hidden",
        //         "Add-Type -AssemblyName System.Drawing;
        //      $pd = New-Object System.Drawing.Printing.PrintDocument;
        //      $pd.PrinterSettings.PrinterName = 'NPIFD3D7B (HP LaserJet MFP M233sdw)';
        //      $pd.add_PrintPage({
        //          param($s, $e)
        //          $e.Graphics.DrawString(
        //              'Hello World',
        //              (New-Object Drawing.Font('Arial', 20)),
        //              [Drawing.Brushes]::Black,
        //              100, 100
        //          )
        //      });
        //      $pd.Print();",
        //     ])
        //     .spawn()
        //     .unwrap();
    }

    Ok(NamedFile::open("main.png")?)
}

#[derive(Deserialize, Serialize, Debug)]
struct LabelInfo {
    kind: i32,
    customer_name: String,
    part_no: String,
    material_name: String,
    qr_string: String,
    is_return: bool,
}
