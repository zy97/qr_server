use actix_files::{Files, NamedFile};
use actix_web::{
    get, http::header::ContentType, middleware, web, App, HttpResponse, HttpServer, Responder,
};
use headless_chrome::{protocol::cdp::Page, Browser, Element, Tab};
use image::{ImageFormat, Luma};
use lazy_static::lazy_static;
use qrcode::QrCode;
use serde::Serialize;
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
        // info!("{:#?}", tera);
        tera
    };
    pub static ref BROWSER:Browser ={
        let browser: Browser = Browser::default().unwrap();
        browser
    };
    pub static ref CTAB:Arc<Tab> ={
        let tab = BROWSER.new_tab().unwrap();
        tab
    };
}
#[get("/hello/{name}")]
async fn greet(name: web::Path<String>) -> impl Responder {
    format!("Hello {name}!")
}
#[get("/qr/{qr_code}")]
async fn get_qr_code(qr_code: web::Path<String>) -> impl Responder {
    let code = QrCode::new(qr_code.as_bytes()).unwrap();
    let image = code.render::<Luma<u8>>().build();

    // image.save("/tmp/qrcode.png").unwrap();
    let mut buffer = Cursor::new(Vec::new());
    image.write_to(&mut buffer, ImageFormat::Png).unwrap();
    HttpResponse::Ok()
        .content_type("image/png")
        .body(buffer.into_inner())
    // format!("Hello {qr_code}!")
}
#[get("/label")]
async fn create_label() -> impl Responder {
    info!("1");
    let code = QrCode::new(r"qr_code.as_bytes()").unwrap();
    let image = code.render::<Luma<u8>>().build();
    image.save("./templates/qr.png").unwrap();
    let mut result = File::create("./templates/result.html").unwrap();
    let product = Product {
        name: "ss".to_string(),
    };
    TEMPLATES
        .render_to(
            "template.html",
            &Context::from_serialize(&product).unwrap(),
            &mut result,
        )
        .unwrap();
    let file = NamedFile::open("./templates/result.html").unwrap();
    info!("2");

    info!("3");
    let tab = CTAB.clone();
    info!("4");
    let current_dir = env::current_dir().unwrap();
    let file_path = current_dir.join("templates/result.html");
    tab.navigate_to(&format!("file:///{}", file_path.display()))
        .unwrap();
    info!("5");
    let elem = tab.find_element("table").unwrap();
    info!("6");
    // let jpeg_data = tab
    //     .capture_screenshot(
    //         Page::CaptureScreenshotFormatOption::Jpeg,
    //         None,
    //         Some(elem.get_box_model().unwrap().content_viewport()),
    //         true,
    //     )
    //     .unwrap();
    let jpeg_data = elem
        .capture_screenshot(Page::CaptureScreenshotFormatOption::Png)
        .unwrap();
    // Save the screenshot to disc
    info!("7");
    std::fs::write("screenshot.png", jpeg_data).unwrap();
    file
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
#[derive(Serialize)]
struct Product {
    name: String,
}
