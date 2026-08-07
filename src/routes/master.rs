use actix_files::NamedFile;
use actix_web::{get, post, web, HttpResponse, Responder};
use base64::{engine::general_purpose, Engine};
use headless_chrome::{Browser, Tab};
use image::{ImageFormat, ImageReader, Luma};
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use std::{
    env,
    fs::File,
    io::{self, Cursor, Write},
    sync::{Arc, LazyLock},
};
use tera::{Context, Tera};
use tracing::info;

use crate::err::CustomError;

static TEMPLATES: LazyLock<Tera> = LazyLock::new(|| {
    let mut tera = Tera::new("templates/template.html").expect("failed to load master template");
    tera.autoescape_on(vec![".html", ".sql"]);
    tera
});

static BROWSER: LazyLock<Browser> = LazyLock::new(|| Browser::default().unwrap());
static CTAB: LazyLock<Arc<Tab>> = LazyLock::new(|| BROWSER.new_tab().unwrap());

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(greet)
        .service(get_qr_code)
        .service(get_barcode)
        .service(create_label);
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

#[get("/barcode/{barcode}")]
async fn get_barcode(barcode: web::Path<String>) -> Result<impl Responder, CustomError> {
    let barcode = barcoders::sym::code128::Code128::new(format!("\u{00C0}{}", barcode)).unwrap();
    let png = barcoders::generators::image::Image::png(5);
    let bytes = png.generate(&barcode.encode()[..]).unwrap();

    Ok(HttpResponse::Ok()
        .content_type("image/png")
        .body(Cursor::new(bytes).into_inner()))
}

#[post("/label")]
async fn create_label(labels: web::Json<Vec<LabelInfo>>) -> Result<impl Responder, CustomError> {
    info!("0");
    let tab = CTAB.clone();

    for label in labels.0 {
        let code = QrCode::new(&label.qr_code)?;
        let mut infos = split_info(&label.qr_code);
        let image = code.render::<Luma<u8>>().build();
        image.save("templates/qr.png")?;
        info!("00");

        let img = ImageReader::open("templates/qr.png")?.decode()?;
        let mut img_bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut img_bytes), ImageFormat::Png)?;
        let base64_string = general_purpose::STANDARD.encode(&img_bytes);
        infos.qr_code = Some(format!("data:image/png;base64,{}", base64_string));

        let mut result = File::create("templates/result_master.html")?;
        TEMPLATES.render_to(
            "template.html",
            &Context::from_serialize(&infos)?,
            &mut result,
        )?;

        let file_path = env::current_dir()?.join("templates/result_master.html");
        let viewport = tab
            .navigate_to(&format!("file:///{}", file_path.display()))?
            .wait_for_element("#app")?;

        let result = viewport.call_js_fn(
            r#"
                function getIdTwice () {
                  return html2canvas(document.getElementById('app')).then(function(canvas) {
                    return canvas.toDataURL();
                  });
                }
            "#,
            vec![],
            true,
        )?;

        match result.value {
            Some(returned_string) => {
                let encoded = returned_string.as_str().unwrap();
                let encoded = encoded.trim_start_matches("data:image/png;base64,");
                let decoded_bytes = general_purpose::STANDARD.decode(encoded).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Failed to decode base64: {error}"),
                    )
                })?;
                let mut file = File::create("master_result.png")?;
                file.write_all(&decoded_bytes)?;
            }
            None => {
                return Err(CustomError::OtherLibraryError(
                    "html2canvas did not return an image".to_string(),
                ))
            }
        }
    }

    Ok(NamedFile::open("master_result.png")?)
}

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
        qr_code: None,
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

#[derive(Serialize)]
struct TemplateData {
    material_no: String,
    lot_no: String,
    order_no: String,
    count: String,
    vender_code: String,
    date: String,
    box_no: String,
    qr_code: Option<String>,
}
