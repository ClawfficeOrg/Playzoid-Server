//! Integration tests for the Redis-backed rate limiter (task 0.4.8/0.4.12).
//!
//! Require a live Redis. Skipped by default (`#[ignore]`).
//! Run with: `cargo test --test rate_limit_integration -- --ignored`
//!
//! Defaults to the Docker dev stack Redis (`redis://127.0.0.1:6379`).
//! Override with `REDIS_URL`.
//!
//! Buckets are keyed by `(class, client ip, window_start)`; each test uses a
//! randomized peer-IP octet per run so parallel/repeated runs never share a
//! bucket within a 60s window.

use actix_web::{App, http::StatusCode, test, web};
use playzoid_server::{
    config::{Config, RateLimitConfig},
    middleware::rate_limit::{RateLimit, RateLimiter},
};
use redis::aio::ConnectionManager;
use std::{
    net::SocketAddr,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const DEFAULT_REDIS_URL: &str = "redis://127.0.0.1:6379";
const JWT_SECRET: &str = "integration-test-jwt-secret-min-32-chars";

/// Peer IP octet, randomized per run so buckets don't collide across runs
/// inside the same 60s window.
fn peer_octet() -> u8 {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    let octet = 2 + (seed.wrapping_add(SEQ.fetch_add(1, Ordering::Relaxed)) % 200) as u8;
    if octet == 0 { 2 } else { octet }
}

async fn redis_mgr() -> ConnectionManager {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| DEFAULT_REDIS_URL.into());
    let client = redis::Client::open(url.as_str()).expect("invalid Redis URL");
    ConnectionManager::new(client)
        .await
        .expect("Redis connection failed — is Docker up?")
}

fn limiter_config(requests: u32, auth_requests: u32) -> RateLimitConfig {
    RateLimitConfig {
        requests,
        auth_requests,
        window_secs: 60,
        auth_window_secs: 60,
        ..RateLimitConfig::default()
    }
}

/// Delete the current fixed-window buckets for `octet` so a stale bucket from
/// an earlier run inside the same 60s window can never fake an exhaustion.
async fn clear_client_buckets(octet: u8) {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| DEFAULT_REDIS_URL.into());
    let client = redis::Client::open(url.as_str()).expect("invalid Redis URL");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("Redis connection failed — is Docker up?");
    let ip = format!("203.0.113.{octet}");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let window_start = now - (now % 60);
    for class in ["default", "auth"] {
        let key = format!("rl:{class}:{ip}:{window_start}");
        let _: Result<(), redis::RedisError> =
            redis::cmd("DEL").arg(key).query_async(&mut conn).await;
    }
}

/// Build the wrapped app via `init_service` inline so the concrete service
/// type stays visible to `call_service` (impl-Trait returns hide it).
macro_rules! limited_app {
    ($mgr:expr, $cfg:expr) => {
        test::init_service(
            App::new()
                .wrap(RateLimit)
                .app_data(web::Data::new(RateLimiter::new($mgr, $cfg)))
                .app_data(web::Data::new(Config {
                    host: "127.0.0.1".into(),
                    port: 8080,
                    database_url: "mysql://test".into(),
                    redis_url: DEFAULT_REDIS_URL.into(),
                    jwt_secret: JWT_SECRET.into(),
                    jwt_expiry_secs: 3600,
                    rate_limit: RateLimitConfig::default(),
                }))
                .route("/ws", web::get().to(|| async { "ws-ok" }))
                .route("/v1/auth/ping", web::get().to(|| async { "auth-ok" }))
                .route("/healthz", web::get().to(|| async { "healthy" })),
        )
        .await
    };
}

/// GET `$uri` from peer ip `203.0.113.$octet`.
macro_rules! get_from {
    ($app:expr, $octet:expr, $uri:expr) => {{
        let addr = format!("203.0.113.{}:50000", $octet)
            .parse::<SocketAddr>()
            .expect("valid addr");
        let req = test::TestRequest::get()
            .uri($uri)
            .peer_addr(addr)
            .to_request();
        test::call_service(&$app, req)
    }};
}

#[actix_web::test]
#[ignore = "requires live Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn blocks_after_budget_exhausted() {
    let octet = peer_octet();
    clear_client_buckets(octet).await;
    let app = limited_app!(redis_mgr().await, limiter_config(3, 1));

    for _ in 0..3 {
        let res = get_from!(app, octet, "/ws").await;
        assert_eq!(res.status(), StatusCode::OK);
    }
    let blocked = get_from!(app, octet, "/ws").await;
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    let headers = blocked.response().headers();
    assert_eq!(headers.get("x-ratelimit-limit").unwrap(), "3");
    assert_eq!(headers.get("x-ratelimit-remaining").unwrap(), "0");
    assert!(headers.get("retry-after").is_some());
}

#[actix_web::test]
#[ignore = "requires live Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn buckets_are_per_client_ip() {
    // Two IPs must not share a bucket: each gets its own budget of 3.
    let octet_a = peer_octet();
    let octet_b = (octet_a.wrapping_add(1)).max(1);
    clear_client_buckets(octet_a).await;
    clear_client_buckets(octet_b).await;
    let app = limited_app!(redis_mgr().await, limiter_config(3, 1));

    for _ in 0..3 {
        assert_eq!(
            get_from!(app, octet_a, "/ws").await.status(),
            StatusCode::OK
        );
    }
    // A different IP is still allowed while A is exhausted.
    assert_eq!(
        get_from!(app, octet_b, "/ws").await.status(),
        StatusCode::OK
    );
    // And A stays blocked.
    assert_eq!(
        get_from!(app, octet_a, "/ws").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[actix_web::test]
#[ignore = "requires live Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn healthz_is_never_limited() {
    let octet = peer_octet();
    clear_client_buckets(octet).await;
    let app = limited_app!(redis_mgr().await, limiter_config(1, 1));

    // Exhaust the default bucket.
    assert_eq!(get_from!(app, octet, "/ws").await.status(), StatusCode::OK);
    assert_eq!(
        get_from!(app, octet, "/ws").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // /healthz is excluded from classification and must stay 200.
    for _ in 0..5 {
        assert_eq!(
            get_from!(app, octet, "/healthz").await.status(),
            StatusCode::OK
        );
    }
}

#[actix_web::test]
#[ignore = "requires live Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn auth_class_uses_tight_budget() {
    let octet = peer_octet();
    clear_client_buckets(octet).await;
    let app = limited_app!(redis_mgr().await, limiter_config(10, 1));

    // Default class allows 10 but auth class only 1 for this IP.
    assert_eq!(
        get_from!(app, octet, "/v1/auth/ping").await.status(),
        StatusCode::OK
    );
    let blocked = get_from!(app, octet, "/v1/auth/ping").await;
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    // The same IP still has default-class budget left.
    assert_eq!(get_from!(app, octet, "/ws").await.status(), StatusCode::OK);
}

#[actix_web::test]
#[ignore = "requires live Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn limiter_disabled_passes_everything() {
    let octet = peer_octet();
    clear_client_buckets(octet).await;
    let cfg = RateLimitConfig {
        enabled: false,
        ..limiter_config(1, 1)
    };
    let app = limited_app!(redis_mgr().await, cfg);

    for _ in 0..5 {
        assert_eq!(get_from!(app, octet, "/ws").await.status(), StatusCode::OK);
    }
}
