pub mod err;
mod routes;

use actix_web::{middleware, App, HttpServer};
use tracing_subscriber::{fmt::Layer, layer::SubscriberExt, FmtSubscriber};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
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
    })
    .bind(("127.0.0.1", 9095))?
    .run()
    .await
}
