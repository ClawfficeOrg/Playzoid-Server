//! Integration tests for the `/leaderboards` endpoints.
//!
//! Require a live MySQL + Redis. Skipped by default (`#[ignore]`).
//! Run with: `cargo test --test leaderboards_integration -- --ignored`
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
        .uri("/auth/register")
        .set_json(serde_json::json!({ "username": username, "password": "pass12345" }))
        .to_request();
    let reg_resp = test::call_service(&app, reg).await;
    assert_eq!(reg_resp.status(), StatusCode::CREATED);
    let body: Value = test::read_body_json(reg_resp).await;
    let player_id = body["id"].as_str().unwrap().to_owned();

    let login = test::TestRequest::post()
        .uri("/auth/login")
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
        .uri("/leaderboards/no-such-board")
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
        .uri("/leaderboards/definitely-not-a-board-xyz")
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
        .uri(&format!("/leaderboards/{board}"))
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
        .uri(&format!("/leaderboards/{board}?page=1&per_page=2"))
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
        .uri(&format!("/leaderboards/{board}?page=2&per_page=2"))
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
            .uri(&format!("/leaderboards/some-board{q}"))
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
        .uri("/leaderboards/any-board/entries")
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
        .uri("/leaderboards/no-such-board-xyz/entries")
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
        .uri(&format!("/leaderboards/{board}/entries"))
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
        .uri(&format!("/leaderboards/{board}/entries"))
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
            .uri(&format!("/leaderboards/{board}/entries"))
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "score": score }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    let req = test::TestRequest::get()
        .uri(&format!("/leaderboards/{board}"))
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
        .uri(&format!("/leaderboards/{board}/entries"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .set_json(serde_json::json!({ "score": 10, "props": {"not": "array"} }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
