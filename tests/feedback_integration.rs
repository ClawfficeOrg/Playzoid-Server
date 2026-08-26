//! Integration tests for `POST /v1/feedback` (player feedback submission).
//!
//! Require a live MySQL + Redis. Skipped by default (`#[ignore]`).
//! Run with: `cargo test --test feedback_integration -- --ignored`
//!
//! Defaults to the Docker dev stack:
//!   mysql://playzoid:password@127.0.0.1:3306/playzoid_dev
//! Override with `DATABASE_URL` / `REDIS_URL` env vars.

use actix_web::{App, http::StatusCode, test, web};
use chrono::{DateTime, Utc};
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

/// Run-scoped marker embedded in each submitted message — the shared dev DB
/// keeps rows across runs and every stored row shares `name = "feedback"`,
/// so assertions filter on a message substring unique to this invocation.
fn run_marker() -> String {
    format!("fb-{}", Uuid::new_v4().simple())
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

macro_rules! feedback_app {
    ($pool:expr, $mgr:expr, $cfg:expr) => {
        test::init_service(
            App::new()
                .app_data(web::Data::new($cfg))
                .app_data(web::Data::new($pool))
                .app_data(web::Data::new($mgr))
                .configure(api::feedback::config),
        )
        .await
    };
}

/// POST one feedback body to `/v1/feedback` as the given bearer token.
macro_rules! post_feedback {
    ($app:expr, $token:expr, $body:expr) => {{
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .insert_header(("Authorization", format!("Bearer {}", $token)))
            .set_json($body)
            .to_request();
        test::call_service(&$app, req)
    }};
}

/// Stored analytics row used to verify what the endpoint actually wrote.
#[derive(Debug, sqlx::FromRow)]
struct StoredFeedbackRow {
    player_id: Option<u64>,
    name: String,
    props: Option<Value>,
    created_at: DateTime<Utc>,
}

/// Newest row whose stored message contains `marker` (parameterized LIKE —
/// markers are hex-only so no wildcard escaping is needed).
async fn fetch_by_marker(pool: &MySqlPool, marker: &str) -> Option<StoredFeedbackRow> {
    let pattern = format!("%{marker}%");
    sqlx::query_as::<_, StoredFeedbackRow>(
        r#"
        SELECT player_id, name, props, created_at
        FROM analytics_events
        WHERE name = 'feedback' AND props LIKE ?
        ORDER BY id DESC
        LIMIT 1
        "#,
    )
    .bind(pattern)
    .fetch_optional(pool)
    .await
    .expect("select stored feedback row")
}

async fn total_feedback_rows(pool: &MySqlPool) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM analytics_events WHERE name = 'feedback'")
            .fetch_one(pool)
            .await
            .expect("count stored feedback rows");
    count
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_feedback_happy_path_stores_row() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let marker = run_marker();
    let message = format!("{marker} Great game, one nitpick though");
    let (player_public_id, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = feedback_app!(pool.clone(), mgr, cfg);

    let resp = post_feedback!(app, token, serde_json::json!({ "message": message })).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["received"], true);

    // Attribution: JWT public id resolves to the internal players.id.
    let (internal_id,): (u64,) = sqlx::query_as("SELECT id FROM players WHERE public_id = ?")
        .bind(&player_public_id)
        .fetch_one(&pool)
        .await
        .expect("fetch internal player id");

    let row = fetch_by_marker(&pool, &marker)
        .await
        .expect("row persisted");
    assert_eq!(row.player_id, Some(internal_id));
    assert_eq!(row.name, "feedback");
    assert_eq!(
        row.props,
        Some(serde_json::json!({ "message": message })),
        "message must be stored verbatim"
    );
    assert!(
        Utc::now() - row.created_at < chrono::Duration::minutes(5),
        "created_at must be DB-stamped close to now"
    );
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_feedback_requires_auth() {
    let (pool, _mgr, cfg) = test_fixtures().await;
    let app = feedback_app!(pool, _mgr, cfg);
    let req = test::TestRequest::post()
        .uri("/v1/feedback")
        .set_json(serde_json::json!({ "message": "hi" }))
        .to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_feedback_blank_message_returns_400_and_writes_nothing() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = feedback_app!(pool.clone(), mgr, cfg);

    // Whitespace-only message passes deserialization but must fail pre-SQL
    // validation with 400; the shared table keeps old rows across runs, so
    // compare the total feedback-row count around the request.
    let before = total_feedback_rows(&pool).await;
    let resp = post_feedback!(app, token, serde_json::json!({ "message": "   " })).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(total_feedback_rows(&pool).await, before);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_feedback_unknown_field_returns_400() {
    let (pool, mgr, cfg) = test_fixtures().await;
    let (_, token) =
        register_and_login(pool.clone(), mgr.clone(), cfg.clone(), &unique_username()).await;
    let app = feedback_app!(pool, mgr, cfg);

    let resp = post_feedback!(
        app,
        token,
        serde_json::json!({ "message": "hi", "rating": 5 })
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
#[ignore = "requires live MySQL + Redis (docker compose -f config/docker-compose.dev.yml up -d)"]
async fn submit_feedback_deleted_player_degrades_anonymous_not_401() {
    // Attribution degrade rule: a soft-deleted caller with a still-valid JWT
    // still gets 201 and their row persists with player_id NULL — account
    // state must never lose feedback text.
    let (pool, mgr, cfg) = test_fixtures().await;
    let marker = run_marker();
    let message = format!("{marker} from a departed player");
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

    let app = feedback_app!(pool.clone(), mgr, cfg);
    let resp = post_feedback!(app, token, serde_json::json!({ "message": message })).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let row = fetch_by_marker(&pool, &marker)
        .await
        .expect("row persisted");
    assert_eq!(row.player_id, None, "deleted caller must store anonymous");
}
