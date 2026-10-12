//! Integration tests for the `/leaderboards` endpoints.
//!
//! Require a live MySQL + Redis. Skipped by default (`#[ignore]`).
//! Run with: `cargo test --test leaderboards_integration -- --ignored`
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

/// Insert a leaderboard entry directly (score submission endpoint is 0.3-3).
async fn seed_entry(pool: &MySqlPool, board: &str, player_id: &str, score: i64) {
    sqlx::query(
        r#"
        INSERT INTO leaderboard_entries (leaderboard_id, player_id, score)
        SELECT l.id, p.id, ?
        FROM leaderboards l
        JOIN players p ON p.public_id = ?
        WHERE l.internal_name = ?
        "#,
    )
    .bind(score)
    .bind(player_id)
    .bind(board)
    .execute(pool)
    .await
    .expect("seed entry");
}

async fn ensure_leaderboard(pool: &MySqlPool, board: &str) {
    sqlx::query("INSERT IGNORE INTO leaderboards (internal_name) VALUES (?)")
        .bind(board)
        .execute(pool)
        .await
        .expect("ensure leaderboard");
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_leaderboard_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .configure(api::leaderboards::config),
    )
    .await;
    let req = test::TestRequest::get()
        .uri("/v1/leaderboards/no-such-board")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_unknown_leaderboard_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::leaderboards::config),
    )
    .await;
    let req = test::TestRequest::get()
        .uri("/v1/leaderboards/definitely-not-a-board-xyz")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_leaderboard_ranks_scores_descending() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let mut seeded: Vec<(String, i64)> = Vec::new();
    for score in [100i64, 500, 300] {
        let username = unique_username();
        let (pid, _) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &username).await;
        seed_entry(&pool, &board, &pid, score).await;
        seeded.push((pid, score));
    }

    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::leaderboards::config),
    )
    .await;
    let req = test::TestRequest::get()
        .uri(&format!("/v1/leaderboards/{board}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    let entries = body["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["score"], 500);
    assert_eq!(entries[0]["rank"], 1);
    assert_eq!(entries[1]["score"], 300);
    assert_eq!(entries[1]["rank"], 2);
    assert_eq!(entries[2]["score"], 100);
    assert_eq!(entries[2]["rank"], 3);
    for e in entries {
        assert!(
            e["playerId"].is_string(),
            "entries must expose camelCase playerId"
        );
    }
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_leaderboard_pagination_ranks_continue_across_pages() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    for score in [10i64, 20, 30] {
        let username = unique_username();
        let (pid, _) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &username).await;
        seed_entry(&pool, &board, &pid, score).await;
    }

    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::leaderboards::config),
    )
    .await;

    let page1 = test::TestRequest::get()
        .uri(&format!("/v1/leaderboards/{board}?page=1&per_page=2"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, page1).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["rank"], 1);
    assert_eq!(entries[0]["score"], 30);
    assert_eq!(entries[1]["rank"], 2);

    let page2 = test::TestRequest::get()
        .uri(&format!("/v1/leaderboards/{board}?page=2&per_page=2"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, page2).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["rank"], 3);
    assert_eq!(entries[0]["score"], 10);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_leaderboard_rejects_invalid_pagination() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .app_data(web::Data::new(mgr))
            .configure(api::leaderboards::config),
    )
    .await;

    for q in ["?page=0", "?per_page=0", "?per_page=101"] {
        let req = test::TestRequest::get()
            .uri(&format!("/v1/leaderboards/some-board{q}"))
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "query {q}");
    }
}

// ── POST /leaderboards/{game_id}/entries ──────────────────────────────────────

macro_rules! leaderboard_app {
    ($pool:expr, $mgr:expr, $cfg:expr) => {
        test::init_service(
            App::new()
                .app_data(web::Data::new($cfg))
                .app_data(web::Data::new($pool))
                .app_data(web::Data::new($mgr))
                .configure(api::leaderboards::config),
        )
        .await
    };
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_entry_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .configure(api::leaderboards::config),
    )
    .await;
    let req = test::TestRequest::post()
        .uri("/v1/leaderboards/any-board/entries")
        .set_json(serde_json::json!({ "score": 100 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_entry_unknown_board_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);
    let req = test::TestRequest::post()
        .uri("/v1/leaderboards/no-such-board-xyz/entries")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 100 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_entry_returns_201_with_rank_and_props() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri(&format!("/v1/leaderboards/{board}/entries"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "score": 1234,
            "props": [{"key": "level", "value": "3"}]
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body: Value = test::read_body_json(resp).await;
    assert!(body["playerId"].is_string());
    assert_eq!(body["score"], 1234);
    assert_eq!(body["rank"], 1);
    assert_eq!(
        body["props"],
        serde_json::json!([{"key": "level", "value": "3"}])
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_entry_duplicate_player_returns_409() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let username = unique_username();
    let (pid, token) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &username).await;

    // First submission seeded directly; the endpoint call must then hit the
    // unique constraint.
    seed_entry(&pool, &board, &pid, 100).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri(&format!("/v1/leaderboards/{board}/entries"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 200 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submitted_entries_appear_ranked_in_get_leaderboard() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let user_a = unique_username();
    let user_b = unique_username();
    let (_, token_a) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &user_a).await;
    let (_, token_b) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &user_b).await;

    let app = leaderboard_app!(pool, mgr, cfg);
    for (token, score) in [(&token_a, 50i64), (&token_b, 500)] {
        let req = test::TestRequest::post()
            .uri(&format!("/v1/leaderboards/{board}/entries"))
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "score": score }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    let req = test::TestRequest::get()
        .uri(&format!("/v1/leaderboards/{board}"))
        .insert_header(("Authorization", format!("Bearer {token_a}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["rank"], 1);
    assert_eq!(entries[0]["score"], 500);
    assert_eq!(entries[1]["rank"], 2);
    assert_eq!(entries[1]["score"], 50);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_entry_rejects_invalid_props() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri(&format!("/v1/leaderboards/{board}/entries"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 10, "props": {"not": "array"} }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ── PUT /leaderboards/{game_id}/entries/{player_id} ───────────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn update_entry_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .configure(api::leaderboards::config),
    )
    .await;
    let req = test::TestRequest::put()
        .uri("/v1/leaderboards/any-board/entries/some-player")
        .set_json(serde_json::json!({ "score": 200 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn update_entry_cross_player_returns_403() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let user_a = unique_username();
    let (pid_a, _token_a) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &user_a).await;
    let (_, token_b) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    seed_entry(&pool, &board, &pid_a, 100).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::put()
        .uri(&format!("/v1/leaderboards/{board}/entries/{pid_a}"))
        .insert_header(("Authorization", format!("Bearer {token_b}")))
        .set_json(serde_json::json!({ "score": 999 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn update_entry_without_existing_entry_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let username = unique_username();
    let (pid, token) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &username).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::put()
        .uri(&format!("/v1/leaderboards/{board}/entries/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 200 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn update_entry_changes_score_and_recomputes_rank() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let user_a = unique_username();
    let user_b = unique_username();
    let (pid_a, token_a) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &user_a).await;
    let (_, token_b) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &user_b).await;

    // A: 100, B: 500 — A ranks 2nd.
    seed_entry(&pool, &board, &pid_a, 100).await;
    let app = leaderboard_app!(pool, mgr, cfg);
    let req = test::TestRequest::post()
        .uri(&format!("/v1/leaderboards/{board}/entries"))
        .insert_header(("Authorization", format!("Bearer {token_b}")))
        .set_json(serde_json::json!({ "score": 500 }))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::CREATED
    );

    // A updates to 1000 → rank 1; B drops to rank 2.
    let req = test::TestRequest::put()
        .uri(&format!("/v1/leaderboards/{board}/entries/{pid_a}"))
        .insert_header(("Authorization", format!("Bearer {token_a}")))
        .set_json(serde_json::json!({ "score": 1000 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["score"], 1000);
    assert_eq!(body["rank"], 1);
    assert_eq!(body["playerId"], pid_a);

    // Verify via GET leaderboard.
    let req = test::TestRequest::get()
        .uri(&format!("/v1/leaderboards/{board}"))
        .insert_header(("Authorization", format!("Bearer {token_a}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    let body: Value = test::read_body_json(resp).await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries[0]["score"], 1000);
    assert_eq!(entries[1]["score"], 500);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn update_entry_keeps_props_when_omitted_and_replaces_when_supplied() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let username = unique_username();
    let (pid, token) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &username).await;
    let app = leaderboard_app!(pool.clone(), mgr, cfg);

    // Create with props via POST.
    let req = test::TestRequest::post()
        .uri(&format!("/v1/leaderboards/{board}/entries"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "score": 10,
            "props": [{"key": "level", "value": "1"}]
        }))
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::CREATED
    );

    // Update score only → props preserved in DB.
    let req = test::TestRequest::put()
        .uri(&format!("/v1/leaderboards/{board}/entries/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 20 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["props"], serde_json::json!(null)); // omitted from response

    let stored: Option<(Option<Value>,)> = sqlx::query_as(
        "SELECT e.props FROM leaderboard_entries e \
         JOIN leaderboards l ON l.id = e.leaderboard_id \
         JOIN players p ON p.id = e.player_id \
         WHERE l.internal_name = ? AND p.public_id = ?",
    )
    .bind(&board)
    .bind(&pid)
    .fetch_optional(&pool)
    .await
    .expect("fetch props");
    assert_eq!(
        stored.unwrap().0,
        Some(serde_json::json!([{"key": "level", "value": "1"}]))
    );

    // Update with new props → replaced.
    let req = test::TestRequest::put()
        .uri(&format!("/v1/leaderboards/{board}/entries/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({
            "score": 30,
            "props": [{"key": "level", "value": "9"}]
        }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(
        body["props"],
        serde_json::json!([{"key": "level", "value": "9"}])
    );
}

// ── Gap-fill tests (task 0.3.16) ──────────────────────────────────────────────

/// Mint an auth-bearing token for an arbitrary subject via the shared JWT
/// secret — used to prove the unknown-player 404 path for leaderboard entries.
fn token_for(cfg: &Config, player_public_id: &str) -> String {
    playzoid_server::services::auth::issue_jwt(&cfg.jwt_secret, player_public_id, 3600)
        .expect("issue token")
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_entry_missing_score_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri("/v1/leaderboards/some-board/entries")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "props": [] }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_entry_rejects_oversized_props_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let big = serde_json::Value::Array(vec![serde_json::Value::String("x".into());
        playzoid_server::services::leaderboards::MAX_PROPS_BYTES]);
    let req = test::TestRequest::post()
        .uri("/v1/leaderboards/some-board/entries")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 100, "props": big }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_entry_unknown_player_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    // A well-formed JWT for a subject never registered as a player.
    let token = token_for(&cfg, &Uuid::new_v4().to_string());
    let app = leaderboard_app!(pool, mgr, cfg);
    let req = test::TestRequest::post()
        .uri(&format!("/v1/leaderboards/{board}/entries"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 100 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn update_entry_missing_score_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::put()
        .uri(&format!("/v1/leaderboards/some-board/entries/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "props": [] }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn update_entry_rejects_oversized_props_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let big = serde_json::Value::Array(vec![serde_json::Value::String("x".into());
        playzoid_server::services::leaderboards::MAX_PROPS_BYTES]);
    let req = test::TestRequest::put()
        .uri(&format!("/v1/leaderboards/some-board/entries/{pid}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 200, "props": big }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn update_entry_unknown_board_returns_404() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (pid, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::put()
        .uri(&format!(
            "/v1/leaderboards/definitely-not-a-board-xyz/entries/{pid}"
        ))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 200 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_leaderboard_empty_board_returns_empty_entries() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::get()
        .uri(&format!("/v1/leaderboards/{board}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    let entries = body["entries"].as_array().expect("entries array");
    assert!(
        entries.is_empty(),
        "expected empty entries, got {entries:?}"
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_leaderboard_page_beyond_data_returns_empty_entries() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    for score in [10i64, 20, 30] {
        let username = unique_username();
        let (pid, _) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &username).await;
        seed_entry(&pool, &board, &pid, score).await;
    }
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::get()
        .uri(&format!("/v1/leaderboards/{board}?page=9&per_page=50"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    let entries = body["entries"].as_array().expect("entries array");
    assert!(
        entries.is_empty(),
        "expected empty entries, got {entries:?}"
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_leaderboard_non_numeric_pagination_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    for q in ["?page=abc", "?per_page=xyz", "?page=1&per_page="] {
        let req = test::TestRequest::get()
            .uri(&format!("/v1/leaderboards/some-board{q}"))
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "query {q}");
    }
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn get_leaderboard_ranks_tied_scores_by_earlier_submission() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    // Two players, equal scores; the first-seeded entry (lower auto-increment
    // id / earlier created_at) must rank above the second.
    let user_a = unique_username();
    let user_b = unique_username();
    let (pid_a, _) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &user_a).await;
    let (pid_b, _) = register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &user_b).await;
    seed_entry(&pool, &board, &pid_a, 100).await;
    seed_entry(&pool, &board, &pid_b, 100).await;

    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    let req = test::TestRequest::get()
        .uri(&format!("/v1/leaderboards/{board}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = test::read_body_json(resp).await;
    let entries = body["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["playerId"], pid_a);
    assert_eq!(entries[0]["rank"], 1);
    assert_eq!(entries[1]["playerId"], pid_b);
    assert_eq!(entries[1]["rank"], 2);
}

// ── Legacy `/leaderboards` alias paths (0.4.1 transition) ─────────────────────

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_entry_via_legacy_path_still_works() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let board = format!("board-{}", &Uuid::new_v4().to_string()[..8]);
    ensure_leaderboard(&pool, &board).await;

    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = leaderboard_app!(pool, mgr, cfg);

    // Submit through the legacy unprefixed mount…
    let req = test::TestRequest::post()
        .uri(&format!("/leaderboards/{board}/entries"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 77 }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // …and read it back through the canonical `/v1` mount: both mounts
    // share the same underlying service and data.
    let req = test::TestRequest::get()
        .uri(&format!("/v1/leaderboards/{board}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = test::read_body_json(resp).await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["score"], 77);
}
