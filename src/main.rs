use actix_web::{web, App, HttpServer};
use tracing::info;
use tracing_actix_web::TracingLogger;

mod api;
mod sockets;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bind = "127.0.0.1:8080";
    info!("Starting Playzoid server on {}", bind);
    HttpServer::new(|| {
        App::new()
            .wrap(TracingLogger::default())
            .configure(api::healthz::config)
            .configure(api::auth::config)
            .configure(api::players::config)
            .configure(api::socket_ticket::config)
            .route("/ws", web::get().to(sockets::ws::ws_index))
    })
    .bind(bind)?
    .run()
    .await
}
