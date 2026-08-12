mod child_cleanup;
#[cfg(any(feature = "master", feature = "typst", feature = "chrome"))]
mod config;
mod designer;
pub mod err;
#[cfg(any(feature = "master", feature = "typst", feature = "chrome"))]
mod print;
mod requests;
mod routes;

use actix_web::{middleware, App, HttpServer};
use tracing_subscriber::{fmt::Layer, layer::SubscriberExt, FmtSubscriber};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // 服务退出时由 OS 连带终止子进程（chrome / typst watch），避免残留
    #[cfg(windows)]
    child_cleanup::setup_kill_on_close_job();
    let file_appender = tracing_appender::rolling::daily("logs", "app.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let file_layer = Layer::new().with_writer(non_blocking);
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .finish()
        .with(file_layer);
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    HttpServer::new(|| {
        App::new()
            .wrap(middleware::Logger::default())
            .configure(routes::configure)
            .configure(designer::configure)
    })
    .bind(("127.0.0.1", 9095))?
    .run()
    .await
}
