//! Integration tests for the `/players` endpoints.
//!
//! Require a live MySQL + Redis. Skipped by default (`#[ignore]`).
//! Run with: `cargo test --test players_integration -- --ignored`
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

/// Register then login using a standalone auth-only app.
/// Returns `(player_public_id, jwt_token)`.
/// The pool/mgr/cfg are cloned so the caller keeps ownership.
async fn register_and_login(
    pool: MySqlPool,
    mgr: ConnectionManager,
    cfg: Config,
    username: &str,
    password: &str,
) -> (String, String) {
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config),
    )
    .await;

    let reg = test::TestRequest::post()
        .uri("/v1/auth/register")
        .set_json(serde_json::json!({ "username": username, "password": password }))
        .to_request();
    let reg_resp = test::call_service(&app, reg).await;
    assert_eq!(
        reg_resp.status(),
        StatusCode::CREATED,
        "register failed for {username}"
    );
    let reg_body: Value = test::read_body_json(reg_resp).await;
    let player_id = reg_body["id"].as_str().unwrap().to_owned();

    let login = test::TestRequest::post()
        .uri("/v1/auth/login")
        .set_json(serde_json::json!({ "username": username, "password": password }))
        .to_request();
    let login_resp = test::call_service(&app, login).await;
    assert_eq!(login_resp.status(), StatusCode::OK);
    let login_body: Value = test::read_body_json(login_resp).await;
    let token = login_body["token"].as_str().unwrap().to_owned();

    (player_id, token)
}

// ── GET /players/{id} ─────────────────────────────────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_player_own_profile_returns_200() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let username = unique_username();
    let (player_id, token) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &username,
        "pass12345",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config)
            .configure(api::players::config),
    )
    .await;

    let req = test::TestRequest::get()
        .uri(&format!("/v1/players/{player_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["id"], player_id);
    assert_eq!(body["username"], username);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_player_nonexistent_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let username = unique_username();
    let (_, token) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &username,
        "pass12345",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config)
            .configure(api::players::config),
    )
    .await;

    let req = test::TestRequest::get()
        .uri("/v1/players/00000000-0000-0000-0000-000000000000")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_player_no_auth_returns_401() {
    let (pool, mgr, cfg) = test_fixtures().await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::players::config),
    )
    .await;

    let req = test::TestRequest::get()
        .uri("/v1/players/some-id")
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

// ── PUT /players/{id} ─────────────────────────────────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn update_own_profile_returns_200() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let username = unique_username();
    let new_username = unique_username();
    let (player_id, token) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &username,
        "pass12345",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config)
            .configure(api::players::config),
    )
    .await;

    let req = test::TestRequest::put()
        .uri(&format!("/v1/players/{player_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "username": &new_username }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["username"], new_username);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn update_other_players_profile_returns_403() {
    let (pool, mgr, cfg) = test_fixtures().await;

    let (player_a_id, _) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &unique_username(),
        "pass12345",
    )
    .await;
    let (_, token_b) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &unique_username(),
        "pass12345",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config)
            .configure(api::players::config),
    )
    .await;

    let req = test::TestRequest::put()
        .uri(&format!("/v1/players/{player_a_id}"))
        .insert_header(("Authorization", format!("Bearer {token_b}")))
        .set_json(serde_json::json!({ "username": unique_username() }))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::FORBIDDEN
    );
}

// ── DELETE /players/{id} ──────────────────────────────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn delete_own_account_returns_204_and_subsequent_get_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let username = unique_username();
    let (player_id, token) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &username,
        "pass12345",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config)
            .configure(api::players::config),
    )
    .await;

    let del = test::TestRequest::delete()
        .uri(&format!("/v1/players/{player_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, del).await.status(),
        StatusCode::NO_CONTENT
    );

    let get = test::TestRequest::get()
        .uri(&format!("/v1/players/{player_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, get).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn delete_other_account_returns_403() {
    let (pool, mgr, cfg) = test_fixtures().await;

    let (player_a_id, _) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &unique_username(),
        "pass12345",
    )
    .await;
    let (_, token_b) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &unique_username(),
        "pass12345",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config)
            .configure(api::players::config),
    )
    .await;

    let req = test::TestRequest::delete()
        .uri(&format!("/v1/players/{player_a_id}"))
        .insert_header(("Authorization", format!("Bearer {token_b}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::FORBIDDEN
    );
}

// ── POST /players/subaccount ──────────────────────────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_subaccount_returns_201_with_parent_id() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let parent_username = unique_username();
    let child_username = unique_username();
    let (parent_id, token) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &parent_username,
        "pass12345",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config)
            .configure(api::players::config),
    )
    .await;

    let req = test::TestRequest::post()
        .uri("/v1/players/subaccount")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "username": &child_username, "password": "childpass123" }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["username"], child_username);
    assert_eq!(body["parent_account_id"], parent_id);
}

// ── GET /players/{id}/subaccounts ─────────────────────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn list_subaccounts_returns_created_children() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let parent_username = unique_username();
    let child_username = unique_username();
    let (parent_id, token) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &parent_username,
        "pass12345",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config)
            .configure(api::players::config),
    )
    .await;

    let sub = test::TestRequest::post()
        .uri("/v1/players/subaccount")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "username": &child_username, "password": "childpass123" }))
        .to_request();
    test::call_service(&app, sub).await;

    let list = test::TestRequest::get()
        .uri(&format!("/v1/players/{parent_id}/subaccounts"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let list_resp = test::call_service(&app, list).await;
    assert_eq!(list_resp.status(), StatusCode::OK);

    let arr: Vec<Value> = test::read_body_json(list_resp).await;
    assert!(
        arr.iter().any(|v| v["username"] == child_username),
        "child not found in subaccounts list: {arr:?}"
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn list_subaccounts_cross_account_returns_403() {
    let (pool, mgr, cfg) = test_fixtures().await;

    let (player_a_id, _) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &unique_username(),
        "pass12345",
    )
    .await;
    let (_, token_b) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &unique_username(),
        "pass12345",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config)
            .configure(api::players::config),
    )
    .await;

    let req = test::TestRequest::get()
        .uri(&format!("/v1/players/{player_a_id}/subaccounts"))
        .insert_header(("Authorization", format!("Bearer {token_b}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::FORBIDDEN
    );
}

// ── Legacy `/players` alias paths (0.4.1 transition) ──────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_player_via_legacy_path_still_works() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let username = unique_username();
    let (player_id, token) = register_and_login(
        pool.clone(),
        mgr.clone(),
        cfg.clone(),
        &username,
        "pass12345",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::auth::config)
            .configure(api::players::config),
    )
    .await;

    // Legacy unprefixed mount must serve the same profile as `/v1`.
    let req = test::TestRequest::get()
        .uri(&format!("/players/{player_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["id"], player_id);
    assert_eq!(body["username"], username);
}
