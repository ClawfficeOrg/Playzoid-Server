use actix_web::{App, HttpServer, web};
use tracing::info;
use tracing_actix_web::TracingLogger;

mod api;
mod config;
mod db;
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

    let cfg_data = web::Data::new(cfg.clone());

    // Build the DB pool. We surface a friendly error but don't gate on a successful
    // initial query — the pool will reconnect lazily.
    let pool = match db::build_pool(&cfg.database_url).await {
        Ok(p) => {
            info!("Database pool initialised");
            Some(p)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to initialise DB pool; continuing in degraded mode");
            None
        }
    };

    info!("Starting Playzoid server on {}", bind);

    HttpServer::new(move || {
        let mut app = App::new()
            .wrap(TracingLogger::default())
            .app_data(cfg_data.clone())
            .configure(api::healthz::config)
            .configure(api::auth::config)
            .configure(api::players::config)
            .configure(api::socket_ticket::config)
            .route("/ws", web::get().to(sockets::ws::ws_index));
        if let Some(p) = pool.clone() {
            app = app.app_data(web::Data::new(p));
        }
        app
    })
    .bind(&bind)?
    .run()
    .await
}
