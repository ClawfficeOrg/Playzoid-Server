use actix_web::{App, HttpServer, web};
use tracing::info;
use tracing_actix_web::TracingLogger;

mod api;
mod config;
mod entities;
mod middleware;
mod services;
mod sockets;

use config::Config;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Load .env if present; missing file is not an error in production.
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = Config::from_env().map_err(|e| {
        tracing::error!(error = %e, "Failed to load configuration");
        std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())
    })?;

    let bind = cfg.bind_addr();
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
    .bind(&bind)?
    .run()
    .await
}
