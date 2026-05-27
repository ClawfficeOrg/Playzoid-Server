//! Integration tests for `POST /auth/register` and `POST /auth/login`.
//!
//! Require a live MySQL + Redis. Skipped by default (`#[ignore]`).
//! Run with: `cargo test --test auth_integration -- --ignored`
//!
//! Defaults to the Docker dev stack:
//!   mysql://playzoid:password@127.0.0.1:3306/playzoid_dev
//! Override with `DATABASE_URL` / `REDIS_URL` env vars.

use actix_web::{App, http::StatusCode, test, web};
use playzoid_server::{api, config::Config, db};
use redis::aio::ConnectionManager;
use serde_json::Value;
use uuid::Uuid;

const DEFAULT_DB_URL: &str = "mysql://playzoid:password@127.0.0.1:3306/playzoid_dev";
const DEFAULT_REDIS_URL: &str = "redis://127.0.0.1:6379";
const JWT_SECRET: &str = "integration-test-jwt-secret-min-32-chars";

async fn test_fixtures() -> (sqlx::MySqlPool, ConnectionManager, Config) {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DB_URL.into());
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| DEFAULT_REDIS_URL.into());
    let pool = db::build_pool(&db_url)
        .await
        .expect("DB connection failed — is Docker up? Run: docker compose -f config/docker-compose.dev.yml up -d");
    let client = redis::Client::open(redis_url.as_str()).expect("invalid Redis URL");
    let mgr = ConnectionManager::new(client)
        .await
        .expect("Redis connection failed — is Docker up?");
    let cfg = Config {
        host: "127.0.0.1".into(),
        port: 8080,
        database_url: db_url,
        redis_url,
        jwt_secret: JWT_SECRET.into(),
        jwt_expiry_secs: 3600,
    };
    (pool, mgr, cfg)
}

fn unique_username() -> String {
    format!("u{}", &Uuid::new_v4().to_string().replace('-', "")[..12])
}

// ── POST /auth/register ───────────────────────────────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn register_creates_player_and_returns_201() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config),
    )
    .await;

    let username = unique_username();
    let req = test::TestRequest::post()
        .uri("/auth/register")
        .set_json(serde_json::json!({ "username": &username, "password": "supersecretpassword" }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["username"], username);
    assert!(body["id"].is_string() && !body["id"].as_str().unwrap().is_empty());
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn register_duplicate_username_returns_409() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config),
    )
    .await;

    let username = unique_username();
    let payload = serde_json::json!({ "username": &username, "password": "supersecretpassword" });

    let r1 = test::TestRequest::post()
        .uri("/auth/register")
        .set_json(&payload)
        .to_request();
    assert_eq!(
        test::call_service(&app, r1).await.status(),
        StatusCode::CREATED
    );

    let r2 = test::TestRequest::post()
        .uri("/auth/register")
        .set_json(&payload)
        .to_request();
    assert_eq!(
        test::call_service(&app, r2).await.status(),
        StatusCode::CONFLICT
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn register_short_password_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/auth/register")
        .set_json(serde_json::json!({ "username": "validuser", "password": "short" }))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::BAD_REQUEST
    );
}

// ── POST /auth/login ──────────────────────────────────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn login_valid_credentials_returns_jwt() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config),
    )
    .await;

    let username = unique_username();
    let password = "supersecretpassword";

    let reg = test::TestRequest::post()
        .uri("/auth/register")
        .set_json(serde_json::json!({ "username": &username, "password": password }))
        .to_request();
    assert_eq!(
        test::call_service(&app, reg).await.status(),
        StatusCode::CREATED
    );

    let req = test::TestRequest::post()
        .uri("/auth/login")
        .set_json(serde_json::json!({ "username": &username, "password": password }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    assert!(body["token"].is_string() && !body["token"].as_str().unwrap().is_empty());
    assert!(body["expires_in"].as_u64().unwrap() > 0);
    assert_eq!(body["player"]["username"], username);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn login_wrong_password_returns_401() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config),
    )
    .await;

    let username = unique_username();
    let reg = test::TestRequest::post()
        .uri("/auth/register")
        .set_json(serde_json::json!({ "username": &username, "password": "correctpassword" }))
        .to_request();
    test::call_service(&app, reg).await;

    let req = test::TestRequest::post()
        .uri("/auth/login")
        .set_json(serde_json::json!({ "username": &username, "password": "wrongpassword!!" }))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn login_unknown_user_returns_401() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/auth/login")
        .set_json(serde_json::json!({ "username": "ghost-no-such-user", "password": "whatever" }))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::UNAUTHORIZED
    );
}
