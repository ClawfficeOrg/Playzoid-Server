//! Integration tests for the `/saves` endpoints.
//!
//! Require a live MySQL + Redis. Skipped by default (`#[ignore]`).
//! Run with: `cargo test --test saves_integration -- --ignored`
//!
//! Defaults to the Docker dev stack:
//!   mysql://playzoid:password@127.0.0.1:3306/playzoid_dev
//! Override with `DATABASE_URL` / `REDIS_URL` env vars.

use actix_web::{App, http::StatusCode, test, web};
use playzoid_server::{
    api,
    config::{Config, RateLimitConfig},
    db,
};
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
        rate_limit: RateLimitConfig::default(),
    };
    (pool, mgr, cfg)
}

fn unique_username() -> String {
    format!("u{}", &Uuid::new_v4().to_string().replace('-', "")[..12])
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

/// Insert a save row directly (`POST /saves` is task 0.3.7).
/// Returns the public id of the inserted save.
async fn seed_save(
    pool: &MySqlPool,
    player_public_id: &str,
    name: &str,
    save: Value,
    metadata: Option<Value>,
    created_at: &str,
) -> String {
    let public_id = Uuid::new_v4().to_string();
    sqlx::query(
        r#"
        INSERT INTO game_saves (public_id, player_id, name, save, metadata, created_at, updated_at)
        SELECT ?, p.id, ?, ?, ?, ?, ?
        FROM players p
        WHERE p.public_id = ?
        "#,
    )
    .bind(&public_id)
    .bind(name)
    .bind(save)
    .bind(metadata)
    .bind(created_at)
    .bind(created_at)
    .bind(player_public_id)
    .execute(pool)
    .await
    .expect("seed save");
    public_id
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn list_saves_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .configure(api::saves::config),
    )
    .await;
    let req = test::TestRequest::get()
        .uri("/v1/saves/some-player")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn list_saves_cross_player_returns_403() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid_a, _) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let (_, token_b) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::saves::config),
    )
    .await;
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid_a}"))
        .insert_header(("Authorization", format!("Bearer {token_b}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn list_saves_unknown_or_deleted_player_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    // Soft-delete the player account via DELETE /players/{id}.
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(mgr))
            .configure(api::saves::config)
            .configure(api::players::config),
    )
    .await;
    let del = test::TestRequest::delete()
        .uri(&format!("/v1/players/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, del).await.status(),
        StatusCode::NO_CONTENT
    );

    // The still-valid JWT maps to a soft-deleted player → service 404.
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn list_saves_empty_array_when_no_saves() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::saves::config),
    )
    .await;
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    let arr = body.as_array().expect("response must be a JSON array");
    assert!(arr.is_empty(), "expected empty array, got {arr:?}");
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn list_saves_returns_blobs_newest_first() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    // Oldest → newest created_at; response must come back descending.
    let blob_1 = serde_json::json!({ "level": 1, "hp": 100 });
    let blob_2 = serde_json::json!({ "level": 2, "hp": 80 });
    let blob_3 = serde_json::json!({ "level": 3, "hp": 42 });
    seed_save(
        &pool,
        &pid,
        "slot-1",
        blob_1.clone(),
        None,
        "2026-08-25 09:00:00",
    )
    .await;
    seed_save(
        &pool,
        &pid,
        "slot-2",
        blob_2.clone(),
        Some(serde_json::json!({ "zone": "level2" })),
        "2026-08-25 10:00:00",
    )
    .await;
    seed_save(
        &pool,
        &pid,
        "slot-3",
        blob_3.clone(),
        Some(serde_json::json!({ "zone": "level3" })),
        "2026-08-25 11:00:00",
    )
    .await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::saves::config),
    )
    .await;
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    let entries = body.as_array().expect("response must be a JSON array");
    assert_eq!(entries.len(), 3);

    // Newest first, full blobs round-trip, camelCase keys, playerId present.
    assert_eq!(entries[0]["name"], "slot-3");
    assert_eq!(entries[0]["save"], blob_3);
    assert_eq!(
        entries[0]["metadata"],
        serde_json::json!({ "zone": "level3" })
    );
    assert_eq!(entries[0]["playerId"], pid);
    assert!(entries[0]["id"].is_string());
    assert!(entries[0]["createdAt"].is_string());
    assert!(entries[0]["updatedAt"].is_string());

    assert_eq!(entries[1]["name"], "slot-2");
    assert_eq!(entries[1]["save"], blob_2);
    assert_eq!(
        entries[1]["metadata"],
        serde_json::json!({ "zone": "level2" })
    );
    assert_eq!(entries[1]["playerId"], pid);

    assert_eq!(entries[2]["name"], "slot-1");
    assert_eq!(entries[2]["save"], blob_1);
    assert_eq!(entries[2]["metadata"], serde_json::Value::Null);
    assert_eq!(entries[2]["playerId"], pid);

    // Explicit ordering assertion — parsed timestamps must be descending.
    let times: Vec<String> = entries
        .iter()
        .map(|e| {
            e["createdAt"]
                .as_str()
                .expect("createdAt string")
                .to_owned()
        })
        .collect();
    assert!(
        times.windows(2).all(|w| w[0] >= w[1]),
        "order was {times:?}"
    );
}

// ── POST /saves ──────────────────────────────────────────────────────────────

macro_rules! saves_app {
    ($pool:expr, $mgr:expr, $cfg:expr) => {
        test::init_service(
            App::new()
                .app_data(web::Data::new($cfg))
                .app_data(web::Data::new($pool))
                .app_data(web::Data::new($mgr))
                .configure(api::saves::config),
        )
        .await
    };
}

fn valid_body() -> Value {
    serde_json::json!({
        "name": "slot-1",
        "save": { "level": 3, "hp": 42 }
    })
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .configure(api::saves::config),
    )
    .await;
    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .set_json(valid_body())
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_creates_and_round_trips() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = saves_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "name": "slot-1",
            "save": { "level": 3, "hp": 42 },
            "metadata": { "zone": "level3" }
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body: Value = test::read_body_json(resp).await;
    let save_id = body["id"].as_str().expect("save id").to_owned();
    assert!(body["playerId"].is_string());
    assert_eq!(body["playerId"], pid);
    assert_eq!(body["name"], "slot-1");
    assert_eq!(body["save"], serde_json::json!({ "level": 3, "hp": 42 }));
    assert_eq!(body["metadata"], serde_json::json!({ "zone": "level3" }));
    assert!(body["createdAt"].is_string());
    assert!(body["updatedAt"].is_string());

    // Round-trip: the created save must appear in GET /saves/{pid}.
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Value = test::read_body_json(resp).await;
    let entries = list.as_array().expect("saves array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], save_id);
    assert_eq!(entries[0]["save"], body["save"]);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_omits_optional_fields() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = saves_app!(pool, mgr, cfg);

    // No playerId → defaults to JWT; no metadata → null.
    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(valid_body())
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["playerId"], pid);
    assert_eq!(body["metadata"], Value::Null);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_cross_player_returns_403() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token_a) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let (pid_b, _) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = saves_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .insert_header(("Authorization", format!("Bearer {token_a}")))
        .set_json(serde_json::json!({
            "name": "slot-1",
            "playerId": pid_b,
            "save": { "hp": 100 }
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_rejects_unknown_fields() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = saves_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "name": "slot-1",
            "save": { "hp": 100 },
            "bogus": true
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_rejects_oversized_save() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = saves_app!(pool, mgr, cfg);

    let big =
        serde_json::json!({ "data": "x".repeat(playzoid_server::services::saves::MAX_SAVE_BYTES) });
    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "name": "slot-1",
            "save": big
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_soft_deleted_player_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    // Soft-delete the player account via DELETE /players/{id}.
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(mgr))
            .configure(api::saves::config)
            .configure(api::players::config),
    )
    .await;
    let del = test::TestRequest::delete()
        .uri(&format!("/v1/players/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, del).await.status(),
        StatusCode::NO_CONTENT
    );

    // The still-valid JWT maps to a soft-deleted player → service 404.
    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(valid_body())
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── GET /saves/{player_id}/{save_id} ─────────────────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_save_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .configure(api::saves::config),
    )
    .await;
    let req = test::TestRequest::get()
        .uri("/v1/saves/player-uuid-1/save-uuid-1")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_save_returns_full_blob() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let save_id = seed_save(
        &pool,
        &pid,
        "slot-1",
        serde_json::json!({ "level": 3, "hp": 42 }),
        Some(serde_json::json!({ "zone": "level3" })),
        "2026-08-25 09:00:00",
    )
    .await;

    let app = saves_app!(pool, mgr, cfg);
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid}/{save_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["id"], save_id);
    assert_eq!(body["playerId"], pid);
    assert_eq!(body["name"], "slot-1");
    assert_eq!(body["save"], serde_json::json!({ "level": 3, "hp": 42 }));
    assert_eq!(body["metadata"], serde_json::json!({ "zone": "level3" }));
    assert!(body["createdAt"].is_string());
    assert!(body["updatedAt"].is_string());
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_save_cross_player_returns_403() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid_a, _) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let (_, token_b) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let app = saves_app!(pool, mgr, cfg);
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid_a}/some-save-id"))
        .insert_header(("Authorization", format!("Bearer {token_b}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_save_unknown_save_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let app = saves_app!(pool, mgr, cfg);
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid}/{}", Uuid::new_v4()))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_save_other_players_save_returns_404() {
    // Save exists, but belongs to a different player → must 404, never leak.
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid_a, token_a) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let (pid_b, _) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let save_id = seed_save(
        &pool,
        &pid_b,
        "slot-b",
        serde_json::json!({ "hp": 1 }),
        None,
        "2026-08-25 09:00:00",
    )
    .await;

    let app = saves_app!(pool, mgr, cfg);
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid_a}/{save_id}"))
        .insert_header(("Authorization", format!("Bearer {token_a}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_save_soft_deleted_player_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let save_id = seed_save(
        &pool,
        &pid,
        "slot-1",
        serde_json::json!({ "hp": 100 }),
        None,
        "2026-08-25 09:00:00",
    )
    .await;

    // Soft-delete the player account via DELETE /players/{id}.
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(mgr))
            .configure(api::saves::config)
            .configure(api::players::config),
    )
    .await;
    let del = test::TestRequest::delete()
        .uri(&format!("/v1/players/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, del).await.status(),
        StatusCode::NO_CONTENT
    );

    // The still-valid JWT maps to a soft-deleted player → service 404.
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid}/{save_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── DELETE /saves/{player_id}/{save_id} ──────────────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn delete_save_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .configure(api::saves::config),
    )
    .await;
    let req = test::TestRequest::delete()
        .uri("/v1/saves/player-uuid-1/save-uuid-1")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn delete_save_removes_and_verifies_via_get() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let target_id = seed_save(
        &pool,
        &pid,
        "slot-1",
        serde_json::json!({ "level": 3, "hp": 42 }),
        None,
        "2026-08-25 09:00:00",
    )
    .await;
    seed_save(
        &pool,
        &pid,
        "slot-2",
        serde_json::json!({ "level": 2, "hp": 80 }),
        None,
        "2026-08-25 10:00:00",
    )
    .await;

    let app = saves_app!(pool, mgr, cfg);

    let del = test::TestRequest::delete()
        .uri(&format!("/v1/saves/{pid}/{target_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, del).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let body = test::read_body(resp).await;
    assert!(body.is_empty(), "204 must have an empty body");

    // The deleted save is gone: GET on it → 404.
    let get = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid}/{target_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, get).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Sibling saves are unaffected.
    let list = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, list).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    let entries = body.as_array().expect("saves array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "slot-2");
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn delete_save_cross_player_returns_403() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid_a, _) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let (_, token_b) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let app = saves_app!(pool, mgr, cfg);
    let req = test::TestRequest::delete()
        .uri(&format!("/v1/saves/{pid_a}/some-save-id"))
        .insert_header(("Authorization", format!("Bearer {token_b}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn delete_save_unknown_save_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let app = saves_app!(pool, mgr, cfg);
    let req = test::TestRequest::delete()
        .uri(&format!("/v1/saves/{pid}/{}", Uuid::new_v4()))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn delete_save_other_players_save_returns_404() {
    // Save exists, but belongs to a different player → must 404, never leak.
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid_a, token_a) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let (pid_b, _) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let save_id = seed_save(
        &pool,
        &pid_b,
        "slot-b",
        serde_json::json!({ "hp": 1 }),
        None,
        "2026-08-25 09:00:00",
    )
    .await;

    let app = saves_app!(pool, mgr, cfg);
    let req = test::TestRequest::delete()
        .uri(&format!("/v1/saves/{pid_a}/{save_id}"))
        .insert_header(("Authorization", format!("Bearer {token_a}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // The other player's save is still intact.
    let get = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid_b}/{save_id}"))
        .insert_header(("Authorization", format!("Bearer {token_a}")))
        .to_request();
    let resp = test::call_service(&app, get).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn delete_save_soft_deleted_player_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let save_id = seed_save(
        &pool,
        &pid,
        "slot-1",
        serde_json::json!({ "hp": 100 }),
        None,
        "2026-08-25 09:00:00",
    )
    .await;

    // Soft-delete the player account via DELETE /players/{id}.
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(mgr))
            .configure(api::saves::config)
            .configure(api::players::config),
    )
    .await;
    let del = test::TestRequest::delete()
        .uri(&format!("/v1/players/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, del).await.status(),
        StatusCode::NO_CONTENT
    );

    // The still-valid JWT maps to a soft-deleted player → service 404.
    let req = test::TestRequest::delete()
        .uri(&format!("/v1/saves/{pid}/{save_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Gap-fill tests (task 0.3.16) ──────────────────────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_rejects_null_save_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = saves_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "name": "slot-1", "save": null }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_rejects_oversized_name_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = saves_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "name": "x".repeat(256),
            "save": { "hp": 100 }
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_accepts_max_length_name_returns_201() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = saves_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "name": "x".repeat(255),
            "save": { "hp": 100 }
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["name"], "x".repeat(255));
    assert_eq!(body["playerId"], pid);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn create_save_rejects_oversized_metadata_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = saves_app!(pool, mgr, cfg);

    // save fits alone, but metadata pushes the combined size over the cap.
    let big_metadata =
        serde_json::json!({ "zone": "x".repeat(playzoid_server::services::saves::MAX_SAVE_BYTES) });
    let req = test::TestRequest::post()
        .uri("/v1/saves")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "name": "slot-1",
            "save": { "hp": 100 },
            "metadata": big_metadata
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn delete_save_twice_second_delete_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let save_id = seed_save(
        &pool,
        &pid,
        "slot-1",
        serde_json::json!({ "hp": 100 }),
        None,
        "2026-08-25 09:00:00",
    )
    .await;

    let app = saves_app!(pool, mgr, cfg);
    let del = test::TestRequest::delete()
        .uri(&format!("/v1/saves/{pid}/{save_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    assert_eq!(
        test::call_service(&app, del).await.status(),
        StatusCode::NO_CONTENT
    );

    // Second delete of the same save → 404 (idempotency boundary).
    let del = test::TestRequest::delete()
        .uri(&format!("/v1/saves/{pid}/{save_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, del).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Legacy `/saves` alias paths (0.4.1 transition) ────────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn save_roundtrip_via_legacy_paths_still_works() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = saves_app!(pool, mgr, cfg);

    // Create through the legacy unprefixed mount…
    let req = test::TestRequest::post()
        .uri("/saves")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "name": "legacy-slot", "save": { "hp": 7 } }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = test::read_body_json(resp).await;
    let save_id = body["id"].as_str().expect("save id").to_owned();

    // …list through the canonical `/v1` mount: both mounts share state.
    let req = test::TestRequest::get()
        .uri(&format!("/v1/saves/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Value = test::read_body_json(resp).await;
    let entries = list.as_array().expect("saves array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], save_id);

    // Delete through the legacy mount.
    let req = test::TestRequest::delete()
        .uri(&format!("/saves/{pid}/{save_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}
