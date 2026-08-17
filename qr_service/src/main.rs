mod agent_ws;
mod child_cleanup;
mod config;
mod designer;
mod print_script;
pub mod err;
#[cfg(feature = "typst")]
mod requests;
mod routes;
mod template_store;

use actix_web::{middleware, App, HttpServer};
use tracing_subscriber::{fmt::Layer, layer::SubscriberExt, FmtSubscriber};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // 服务退出时由 OS 连带终止子进程（chrome / typst watch），避免残留
    #[cfg(windows)]
    child_cleanup::setup_kill_on_close_job();
    // 初始化模板库：空库时用仓库自带模板播种默认模板，并同步渲染缓存文件
    if let Err(err) = template_store::init() {
        tracing::warn!(error = %err, "模板库初始化失败，模板管理功能不可用");
    }
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
            // 页面/接口内容实时来自磁盘与数据库，禁止浏览器启发式缓存
            .wrap(middleware::DefaultHeaders::new().add(("Cache-Control", "no-cache")))
            .configure(routes::configure)
            .configure(designer::configure)
            .configure(agent_ws::configure)
    })
    .bind((config::CONFIG.server.host.as_str(), config::CONFIG.server.port))?
    .run()
    .await
}
