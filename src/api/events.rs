//! `POST /v1/events` — batched analytics-event ingest (fire-and-forget).
//!
//! The body is a **bare JSON array** of `{ "name": ..., "props": ...? }`
//! objects (no wrapper object, no client timestamps — `created_at` is
//! DB-stamped). Validation is synchronous and whole-batch: any invalid event
//! rejects everything with 400 *before* the database is touched.
//!
//! Accepted batches answer `202 Accepted` with `{"accepted": <n>}` once the
//! rows are written. A post-validation database failure is logged
//! server-side but **still** answers 202: analytics loss is tolerable by
//! definition and clients must never block on DB health (fire-and-forget
//! contract, see `docs/memory.md`).
//!
//! Auth-guarded via [`AuthenticatedUser`]; the JWT identity is attributed
//! best-effort (unknown/deleted callers store anonymous rows rather than
//! failing). No legacy alias mount: this route is born after the 0.4.1
//! prefix-parity pass (`/v1/socket-tickets` precedent).

use crate::middleware::auth::AuthenticatedUser;
use crate::services::events::{self as events_svc, EventInput, EventsServiceError};
use actix_web::{HttpResponse, web};
use sqlx::MySqlPool;

/// Register the events route under its canonical `/v1` spelling.
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/v1/events").route("", web::post().to(post_events)));
}

/// Ingest one batch of analytics events.
///
/// Guard order is cheapest-first: auth (401) → pool presence (503) → JSON
/// deserialization + pre-SQL validation (400) → insert. Any post-validation
/// database failure is logged and answered 202 regardless — fire-and-forget.
#[tracing::instrument(skip(pool, user, body))]
async fn post_events(
    user: AuthenticatedUser,
    pool: Option<web::Data<MySqlPool>>,
    body: web::Json<Vec<EventInput>>,
) -> HttpResponse {
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };

    match events_svc::ingest_events(pool.get_ref(), &user.player_public_id, &body).await {
        Ok(accepted) => HttpResponse::Accepted().json(serde_json::json!({ "accepted": accepted })),
        Err(EventsServiceError::Invalid(msg)) => HttpResponse::BadRequest().json(error_body(&msg)),
        Err(e) => {
            // Fire-and-forget: the batch is lost, but the client still gets
            // its 202 — observability gap closed by /metrics in task 0.4.9.
            tracing::error!(error = ?e, batch_size = body.len(), "post_events: batch dropped");
            HttpResponse::Accepted().json(serde_json::json!({ "accepted": body.len() }))
        }
    }
}

fn error_body(msg: &str) -> serde_json::Value {
    serde_json::json!({ "error": msg })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, RateLimitConfig};
    use crate::services::auth as auth_svc;
    use crate::services::events::{MAX_BATCH_EVENTS, MAX_PROPS_BYTES};
    use actix_web::{App, http::StatusCode, test, web};
    use std::time::Duration;

    const SECRET: &str = "test-secret-test-secret-test-secret-0000";

    fn stub_config() -> Config {
        Config {
            host: "127.0.0.1".into(),
            port: 8080,
            database_url: "mysql://test".into(),
            redis_url: "redis://test".into(),
            jwt_secret: SECRET.into(),
            jwt_expiry_secs: 3600,
            rate_limit: RateLimitConfig::default(),
        }
    }

    fn valid_token(player_id: &str) -> String {
        auth_svc::issue_jwt(SECRET, player_id, 3600).expect("issue token")
    }

    /// Lazy pool pointed at a port nothing listens on — every query fails
    /// fast, proving which checks run before any SQL and that DB failures
    /// never surface as 5xx for this endpoint.
    fn dead_pool() -> MySqlPool {
        sqlx::mysql::MySqlPoolOptions::new()
            .acquire_timeout(Duration::from_secs(2))
            .connect_lazy("mysql://test:test@127.0.0.1:1/test")
            .expect("lazy pool creation should not fail")
    }

    #[actix_web::test]
    async fn post_events_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
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
    async fn post_events_without_pool_returns_503() {
        // Valid token + body; the pool check fires before the service call.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/events")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!([{ "name": "evt" }]))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn post_events_malformed_json_returns_400() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
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
    async fn post_events_non_array_body_returns_400() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/events")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "name": "evt" }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_events_rejects_unknown_field() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/events")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!([{ "name": "evt", "bogus": 1 }]))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_events_blank_name_returns_400_pre_sql() {
        // Validation runs before any SQL, so a dead pool is safe here: a 400
        // proves the request never reached the database layer.
        let token = valid_token("player-uuid-1");
        let pool = dead_pool();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(pool))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/events")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!([{ "name": "   " }]))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_events_over_64_char_name_returns_400_pre_sql() {
        let token = valid_token("player-uuid-1");
        let pool = dead_pool();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(pool))
                .configure(config),
        )
        .await;
        let too_long = "n".repeat(65);
        let req = test::TestRequest::post()
            .uri("/v1/events")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!([{ "name": too_long }]))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_events_oversized_props_returns_400_pre_sql() {
        let token = valid_token("player-uuid-1");
        let pool = dead_pool();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(pool))
                .configure(config),
        )
        .await;
        let big = serde_json::json!({ "data": "x".repeat(MAX_PROPS_BYTES) });
        let req = test::TestRequest::post()
            .uri("/v1/events")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!([{ "name": "evt", "props": big }]))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_events_over_max_batch_returns_400_pre_sql() {
        let token = valid_token("player-uuid-1");
        let pool = dead_pool();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(pool))
                .configure(config),
        )
        .await;
        let batch: Vec<_> = (0..=MAX_BATCH_EVENTS)
            .map(|i| serde_json::json!({ "name": format!("evt-{i}") }))
            .collect();
        let req = test::TestRequest::post()
            .uri("/v1/events")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(batch)
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_events_empty_batch_returns_400_pre_sql() {
        let token = valid_token("player-uuid-1");
        let pool = dead_pool();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(pool))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/events")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!([]))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_events_dead_db_still_answers_202() {
        // Fire-and-forget contract: validation passes, then the insert fails
        // against the dead pool — the client must still get 202 with the
        // accepted count, never a 5xx.
        let token = valid_token("player-uuid-1");
        let pool = dead_pool();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(pool))
                .configure(config),
        )
        .await;
        let batch = serde_json::json!([
            { "name": "level_complete", "props": { "level": 7 } },
            { "name": "session_start" }
        ]);
        let req = test::TestRequest::post()
            .uri("/v1/events")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(batch)
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["accepted"], 2);
    }
}
