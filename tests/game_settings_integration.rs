//! Integration tests for the `/v1/games/{game_id}/settings` endpoints.
//!
//! Require a live MySQL + Redis. Skipped by default (`#[ignore]`).
//! Run with: `cargo test --test game_settings_integration -- --ignored`
//!
//! Defaults to the Docker dev stack:
//!   mysql://playzoid:password@127.0.0.1:3306/playzoid_dev
//! Override with `DATABASE_URL` / `REDIS_URL` env vars.

use actix_web::{App, http::StatusCode, test, web};
use playzoid_server::{api, config::Config, db};
use redis::aio::ConnectionManager;
use serde_json::Value;
use sqlx::MySqlPool;
use uuid::Uuid;

const DEFAULT_DB_URL: &str = "mysql://playzoid:password@127.0.0.1:3306/playzoid_dev";
const DEFAULT_REDIS_URL: &str = "redis://127.0.0.1:6379";
const JWT_SECRET: &str = "integration-test-jwt-secret-min-32-chars";

async fn test_fixtures() -> (MySqlPool, ConnectionManager, Config) {
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

/// Unique per-run game id — the shared dev DB keeps rows across runs.
fn unique_game_id() -> String {
    format!("settings-it-{}", Uuid::new_v4())
}

/// Register + login via a standalone auth-only app.
async fn register_and_login(
    pool: MySqlPool,
    mgr: ConnectionManager,
    cfg: Config,
    username: &str,
) -> (String, String) {
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config),
    )
    .await;

    let reg = test::TestRequest::post()
        .uri("/v1/auth/register")
        .set_json(serde_json::json!({ "username": username, "password": "pass12345" }))
        .to_request();
    let reg_resp = test::call_service(&app, reg).await;
    assert_eq!(reg_resp.status(), StatusCode::CREATED);
    let body: Value = test::read_body_json(reg_resp).await;
    let player_id = body["id"].as_str().unwrap().to_owned();

    let login = test::TestRequest::post()
        .uri("/v1/auth/login")
        .set_json(serde_json::json!({ "username": username, "password": "pass12345" }))
        .to_request();
    let login_resp = test::call_service(&app, login).await;
    assert_eq!(login_resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(login_resp).await;
    let token = body["token"].as_str().unwrap().to_owned();

    (player_id, token)
}

macro_rules! settings_app {
    ($pool:expr, $mgr:expr, $cfg:expr) => {
        test::init_service(
            App::new()
                .app_data(web::Data::new($cfg))
                .app_data(web::Data::new($pool))
                .app_data(web::Data::new($mgr))
                .configure(api::game_settings::config),
        )
        .await
    };
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .configure(api::game_settings::config),
    )
    .await;
    let req = test::TestRequest::get()
        .uri("/v1/games/some-game/settings")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn put_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .configure(api::game_settings::config),
    )
    .await;
    let req = test::TestRequest::put()
        .uri("/v1/games/some-game/settings")
        .set_json(serde_json::json!({ "config": { "a": 1 } }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_unknown_game_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = settings_app!(pool, mgr, cfg);

    let req = test::TestRequest::get()
        .uri(&format!("/v1/games/{}/settings", unique_game_id()))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn put_then_get_round_trips_json() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let game_id = unique_game_id();
    let app = settings_app!(pool, mgr, cfg);

    // Milestone review step 5: PUT settings → GET settings round-trips JSON.
    let config = serde_json::json!({
        "difficulty": "hard",
        "lives": 3,
        "powerups": [ "magnet", "shield" ]
    });
    let req = test::TestRequest::put()
        .uri(&format!("/v1/games/{game_id}/settings"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "config": config }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["gameId"], game_id);
    assert_eq!(body["config"], config);
    assert!(body["createdAt"].is_string());
    assert!(body["updatedAt"].is_string());

    // GET returns the identical stored shape.
    let req = test::TestRequest::get()
        .uri(&format!("/v1/games/{game_id}/settings"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let fetched: Value = test::read_body_json(resp).await;
    assert_eq!(fetched, body);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn put_upserts_existing_game() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let game_id = unique_game_id();
    let app = settings_app!(pool, mgr, cfg);

    let first = serde_json::json!({ "difficulty": "easy", "lives": 9 });
    let req = test::TestRequest::put()
        .uri(&format!("/v1/games/{game_id}/settings"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "config": first }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    let created_at = body["createdAt"].as_str().expect("createdAt").to_owned();

    // Second PUT replaces the config but keeps the row's created_at.
    let second = serde_json::json!({ "difficulty": "hard", "lives": 1 });
    let req = test::TestRequest::put()
        .uri(&format!("/v1/games/{game_id}/settings"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "config": second }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["gameId"], game_id);
    assert_eq!(body["config"], second);
    assert_eq!(
        body["createdAt"].as_str().expect("createdAt"),
        created_at,
        "upsert must not reset created_at"
    );

    // GET shows the new value only.
    let req = test::TestRequest::get()
        .uri(&format!("/v1/games/{game_id}/settings"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let fetched: Value = test::read_body_json(resp).await;
    assert_eq!(fetched["config"], second);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn put_malformed_body_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = settings_app!(pool, mgr, cfg);

    let req = test::TestRequest::put()
        .uri(&format!("/v1/games/{}/settings", unique_game_id()))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .insert_header(("Content-Type", "application/json"))
        .set_payload("{ not valid json")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn put_null_config_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = settings_app!(pool, mgr, cfg);

    let req = test::TestRequest::put()
        .uri(&format!("/v1/games/{}/settings", unique_game_id()))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "config": null }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn put_oversized_config_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = settings_app!(pool, mgr, cfg);

    let big = serde_json::json!({
        "data": "x".repeat(playzoid_server::services::game_settings::MAX_CONFIG_BYTES)
    });
    let req = test::TestRequest::put()
        .uri(&format!("/v1/games/{}/settings", unique_game_id()))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "config": big }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn put_over_64_char_game_id_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = settings_app!(pool, mgr, cfg);

    let too_long = "g".repeat(65);
    let req = test::TestRequest::put()
        .uri(&format!("/v1/games/{too_long}/settings"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "config": { "a": 1 } }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn put_get_accepts_exactly_64_char_game_id() {
    // Passing side of the VARCHAR(64) boundary: 64 chars must be accepted
    // and round-trip (only >64 is rejected pre-SQL).
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let game_id = "g".repeat(64);
    let app = settings_app!(pool, mgr, cfg);

    let req = test::TestRequest::put()
        .uri(&format!("/v1/games/{game_id}/settings"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "config": { "boundary": true } }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["gameId"], game_id);

    let req = test::TestRequest::get()
        .uri(&format!("/v1/games/{game_id}/settings"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
}
