//! Integration tests for `POST /v1/events` (analytics-event ingest).
//!
//! Require a live MySQL + Redis. Skipped by default (`#[ignore]`).
//! Run with: `cargo test --test events_integration -- --ignored`
//!
//! Defaults to the Docker dev stack:
//!   mysql://playzoid:password@127.0.0.1:3306/playzoid_dev
//! Override with `DATABASE_URL` / `REDIS_URL` env vars.

use actix_web::{App, http::StatusCode, test, web};
use chrono::{DateTime, Utc};
use playzoid_server::{
    api,
    config::{Config, RateLimitConfig},
    db,
    services::events::MAX_BATCH_EVENTS,
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

/// Run-scoped event-name prefix — the shared dev DB keeps rows across runs,
/// so every assertion filters on names unique to this test invocation.
/// Prefix is `it-<32 hex>`; suffixes keep total length ≤ VARCHAR(64).
fn run_prefix() -> String {
    format!("it-{}", Uuid::new_v4().simple())
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

macro_rules! events_app {
    ($pool:expr, $mgr:expr, $cfg:expr) => {
        test::init_service(
            App::new()
                .app_data(web::Data::new($cfg))
                .app_data(web::Data::new($pool))
                .app_data(web::Data::new($mgr))
                .configure(api::events::config),
        )
        .await
    };
}

/// POST one batch to `/v1/events` as the given bearer token.
macro_rules! post_batch {
    ($app:expr, $token:expr, $body:expr) => {{
        let req = test::TestRequest::post()
            .uri("/v1/events")
            .insert_header(("Authorization", format!("Bearer {}", $token)))
            .set_json($body)
            .to_request();
        test::call_service(&$app, req)
    }};
}

/// Stored analytics row used to verify what the endpoint actually wrote.
#[derive(Debug, sqlx::FromRow)]
struct StoredEventRow {
    player_id: Option<u64>,
    name: String,
    props: Option<Value>,
    created_at: DateTime<Utc>,
}

async fn fetch_by_name(pool: &MySqlPool, name: &str) -> Option<StoredEventRow> {
    sqlx::query_as::<_, StoredEventRow>(
        r#"
        SELECT player_id, name, props, created_at
        FROM analytics_events
        WHERE name = ?
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .expect("select stored event row")
}

async fn count_by_prefix(pool: &MySqlPool, prefix: &str) -> i64 {
    let pattern = format!("{prefix}%");
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM analytics_events WHERE name LIKE ?")
            .bind(pattern)
            .fetch_one(pool)
            .await
            .expect("count stored event rows");
    count
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn post_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg))
            .app_data(web::Data::new(pool))
            .configure(api::events::config),
    )
    .await;
    let req = test::TestRequest::post()
        .uri("/v1/events")
        .set_json(serde_json::json!([{ "name": "evt" }]))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn post_single_event_persists_with_player_attribution() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let prefix = run_prefix();
    let name = format!("{prefix}-single");
    let (player_public_id, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = events_app!(pool.clone(), mgr, cfg);

    let props = serde_json::json!({ "level": 7, "score": 1234 });
    let resp = post_batch!(
        app,
        token,
        serde_json::json!([{ "name": name, "props": props }])
    )
    .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["accepted"], 1);

    // Attribution: JWT public id resolves to the internal players.id.
    let (internal_id,): (u64,) = sqlx::query_as("SELECT id FROM players WHERE public_id = ?")
        .bind(&player_public_id)
        .fetch_one(&pool)
        .await
        .expect("fetch internal player id");

    let row = fetch_by_name(&pool, &name).await.expect("row persisted");
    assert_eq!(row.player_id, Some(internal_id));
    assert_eq!(row.name, name);
    assert_eq!(row.props, Some(props));
    assert!(
        Utc::now() - row.created_at < chrono::Duration::minutes(5),
        "created_at must be DB-stamped close to now"
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn post_batch_persists_all_rows_in_one_response() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let prefix = run_prefix();
    let names: Vec<String> = (0..3).map(|i| format!("{prefix}-batch-{i}")).collect();
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = events_app!(pool.clone(), mgr, cfg);

    let batch: Vec<Value> = names
        .iter()
        .map(|n| serde_json::json!({ "name": n }))
        .collect();
    let resp = post_batch!(app, token, Value::Array(batch)).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["accepted"], 3);

    assert_eq!(count_by_prefix(&pool, &prefix).await, 3);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn post_null_and_missing_props_stored_as_null() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let prefix = run_prefix();
    let explicit_null = format!("{prefix}-null");
    let omitted = format!("{prefix}-omitted");
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = events_app!(pool.clone(), mgr, cfg);

    let resp = post_batch!(
        app,
        token,
        serde_json::json!([
            { "name": explicit_null, "props": null },
            { "name": omitted }
        ])
    )
    .await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    for name in [&explicit_null, &omitted] {
        let row = fetch_by_name(&pool, name).await.expect("row persisted");
        assert_eq!(row.props, None, "{name}: props must be SQL NULL");
    }
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn post_invalid_event_writes_nothing() {
    // Whole-batch atomicity: one bad event among good ones rejects the
    // entire batch with 400 and writes zero rows.
    let (pool, mgr, cfg) = test_fixtures().await;
    let prefix = run_prefix();
    let good = format!("{prefix}-good");
    let before = count_by_prefix(&pool, &prefix).await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = events_app!(pool.clone(), mgr, cfg);

    let resp = post_batch!(
        app,
        token,
        serde_json::json!([
            { "name": good },
            { "name": "   " }
        ])
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let after = count_by_prefix(&pool, &prefix).await;
    assert_eq!(after, before, "rejected batch must write no rows");
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn post_empty_batch_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = events_app!(pool, mgr, cfg);

    let resp = post_batch!(app, token, serde_json::json!([])).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn post_over_max_batch_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let batch: Vec<Value> = (0..=MAX_BATCH_EVENTS)
        .map(|i| serde_json::json!({ "name": format!("evt-{i}") }))
        .collect();
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = events_app!(pool, mgr, cfg);

    let resp = post_batch!(app, token, Value::Array(batch)).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn post_malformed_json_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = events_app!(pool, mgr, cfg);

    let req = test::TestRequest::post()
        .uri("/v1/events")
        .insert_header(("Authorization", format!("Bearer {token}")))
        .insert_header(("Content-Type", "application/json"))
        .set_payload("{ not json")
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn boundary_exact_limits_accepted() {
    // Passing side of every cap at once: exactly MAX_BATCH_EVENTS events,
    // each name exactly 64 chars (VARCHAR(64)), each props serialized to
    // exactly 4 KiB.
    let (pool, mgr, cfg) = test_fixtures().await;
    let prefix = run_prefix();
    let props = serde_json::json!({ "d": "x".repeat(playzoid_server::services::events::MAX_PROPS_BYTES - 8) });
    let serialized = serde_json::to_string(&props).expect("serialize");
    assert_eq!(
        serialized.len(),
        playzoid_server::services::events::MAX_PROPS_BYTES
    );

    let names: Vec<String> = (0..MAX_BATCH_EVENTS as u64)
        .map(|i| format!("{prefix}-{:028}", i))
        .collect();
    for name in &names {
        assert_eq!(name.chars().count(), 64);
    }

    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = events_app!(pool.clone(), mgr, cfg);

    let batch: Vec<Value> = names
        .iter()
        .map(|n| serde_json::json!({ "name": n, "props": props }))
        .collect();
    let resp = post_batch!(app, token, Value::Array(batch)).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["accepted"], MAX_BATCH_EVENTS);

    assert_eq!(
        count_by_prefix(&pool, &prefix).await,
        MAX_BATCH_EVENTS as i64
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn deleted_player_events_anonymized_not_404() {
    // Fire-and-forget attribution: a soft-deleted caller still gets 202 and
    // their events persist with player_id NULL — account state must never
    // break telemetry intake.
    let (pool, mgr, cfg) = test_fixtures().await;
    let prefix = run_prefix();
    let name = format!("{prefix}-anon");
    let (player_public_id, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;

    // Soft-delete own account first.
    let delete_app = test::init_service(
        App::new()
            .app_data(web::Data::new(cfg.clone()))
            .app_data(web::Data::new(pool.clone()))
            .app_data(web::Data::new(mgr.clone()))
            .configure(api::players::config),
    )
    .await;
    let req = test::TestRequest::delete()
        .uri(&format!("/v1/players/{player_public_id}"))
        .insert_header(("Authorization", format!("Bearer {token}")))
        .to_request();
    let resp = test::call_service(&delete_app, req).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let app = events_app!(pool.clone(), mgr, cfg);
    let resp = post_batch!(app, token, serde_json::json!([{ "name": name }])).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let row = fetch_by_name(&pool, &name).await.expect("row persisted");
    assert_eq!(row.player_id, None, "deleted caller must store anonymous");
}
