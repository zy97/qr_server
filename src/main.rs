use std::io::Cursor;

use actix_web::{get, web, App, HttpResponse, HttpServer, Responder};
use image::{ImageFormat, Luma};
use qrcode::QrCode;

#[get("/hello/{name}")]
async fn greet(name: web::Path<String>) -> impl Responder {
    format!("Hello {name}!")
}
#[get("/qr/{qr_code}")]
async fn get_qr_code(qr_code: web::Path<String>) -> impl Responder {
    // Encode some data into bits.
    let code = QrCode::new(qr_code.as_bytes()).unwrap();

    // Render the bits into an image.
    let image = code.render::<Luma<u8>>().build();

    // Save the image.

    // image.save("/tmp/qrcode.png").unwrap();
    let mut buffer = Cursor::new(Vec::new());
    image.write_to(&mut buffer, ImageFormat::Png).unwrap();
    HttpResponse::Ok()
        .content_type("image/png")
        .body(buffer.into_inner())
    // format!("Hello {qr_code}!")
}

#[actix_web::main] // or #[tokio::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| App::new().service(greet).service(get_qr_code))
        .bind(("127.0.0.1", 8080))?
        .run()
        .await
}
