use actix_web::{App, HttpServer, web};
use playzoid_server::{api, config::Config, db, sockets};
use tracing::info;
use tracing_actix_web::TracingLogger;

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

    // Build the DB pool — degraded mode if unavailable.
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

    // Build the Redis connection manager — caching disabled if unavailable.
    let redis_mgr = match redis::Client::open(cfg.redis_url.as_str()) {
        Ok(client) => match redis::aio::ConnectionManager::new(client).await {
            Ok(mgr) => {
                info!("Redis connection manager initialised");
                Some(mgr)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to connect to Redis; caching disabled");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "Invalid Redis URL; caching disabled");
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
        if let Some(r) = redis_mgr.clone() {
            app = app.app_data(web::Data::new(r));
        }
        app
    })
    .bind(&bind)?
    .run()
    .await
}
