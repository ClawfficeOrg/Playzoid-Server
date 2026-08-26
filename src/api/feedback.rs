//! `POST /v1/feedback` — player feedback submission.
//!
//! Body is `{ "message": <1..=1000 chars> }`; unknown fields are rejected
//! with 400. Feedback is **user content**, so unlike the analytics ingest
//! (`POST /v1/events`) a post-validation database failure is answered with
//! an honest `500 {"error": "internal error"}` — never a fake success. The
//! failure details are logged server-side only; the response body stays
//! generic so storage internals never leak to clients.
//!
//! Storage reuses the append-only `analytics_events` table (row:
//! `name = "feedback"`, `props = { "message": ... }`); see `docs/memory.md`.
//! Auth-guarded via [`AuthenticatedUser`] with best-effort attribution
//! (unknown/deleted callers store anonymous rows). No legacy alias mount:
//! this route is born after the 0.4.1 prefix-parity pass.

use crate::middleware::auth::AuthenticatedUser;
use crate::services::feedback::{self as feedback_svc, FeedbackInput, FeedbackServiceError};
use actix_web::{HttpResponse, web};
use sqlx::MySqlPool;

/// Register the feedback route under its canonical `/v1` spelling.
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/v1/feedback").route("", web::post().to(post_feedback)));
}

/// Store one player-feedback message.
///
/// Guard order is cheapest-first: auth (401) → pool presence (503) → JSON
/// deserialization + pre-SQL validation (400) → insert. Success answers
/// `201 Created {"received": true}`; a database failure answers 500 and the
/// feedback is lost (client may retry).
#[tracing::instrument(skip(pool, user, body))]
async fn post_feedback(
    user: AuthenticatedUser,
    pool: Option<web::Data<MySqlPool>>,
    body: web::Json<FeedbackInput>,
) -> HttpResponse {
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };

    match feedback_svc::submit_feedback(pool.get_ref(), &user.player_public_id, &body).await {
        Ok(()) => HttpResponse::Created().json(serde_json::json!({ "received": true })),
        Err(FeedbackServiceError::Invalid(msg)) => {
            HttpResponse::BadRequest().json(error_body(&msg))
        }
        Err(e) => {
            // Honest failure: user content must not be silently dropped the
            // way analytics batches are (fire-and-forget divergence).
            tracing::error!(error = ?e, "post_feedback: insert failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

fn error_body(msg: &str) -> serde_json::Value {
    serde_json::json!({ "error": msg })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::services::auth as auth_svc;
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
        }
    }

    fn valid_token(player_id: &str) -> String {
        auth_svc::issue_jwt(SECRET, player_id, 3600).expect("issue token")
    }

    /// Lazy pool pointed at a port nothing listens on — every query fails
    /// fast, proving which checks run before any SQL.
    fn dead_pool() -> MySqlPool {
        sqlx::mysql::MySqlPoolOptions::new()
            .acquire_timeout(Duration::from_secs(2))
            .connect_lazy("mysql://test:test@127.0.0.1:1/test")
            .expect("lazy pool creation should not fail")
    }

    #[actix_web::test]
    async fn post_feedback_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .set_json(serde_json::json!({ "message": "hi" }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn post_feedback_invalid_token_returns_401() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .insert_header(("Authorization", "Bearer not.a.real.jwt"))
            .set_json(serde_json::json!({ "message": "hi" }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn post_feedback_without_pool_returns_503() {
        // Valid token + body; the pool check fires before the service call.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "message": "hi" }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn post_feedback_malformed_json_returns_400() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .insert_header(("Content-Type", "application/json"))
            .set_payload("{ not json")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_feedback_non_object_body_returns_400() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!("just a string"))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_feedback_rejects_unknown_field() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "message": "hi", "rating": 5 }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_feedback_blank_message_returns_400_pre_sql() {
        // Validation runs before any SQL, so a dead pool is safe here: a 400
        // proves the request never reached the database layer.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(dead_pool()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "message": "   " }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_feedback_over_1000_char_message_returns_400_pre_sql() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(dead_pool()))
                .configure(config),
        )
        .await;
        let too_long = "m".repeat(crate::services::feedback::MAX_MESSAGE_CHARS + 1);
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "message": too_long }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_feedback_encoded_props_over_4kib_returns_400_pre_sql() {
        // Length-valid (1000 chars) but escape-heavy message whose JSON
        // encoding blows past MAX_PROPS_BYTES → rejected pre-SQL.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(dead_pool()))
                .configure(config),
        )
        .await;
        let escape_heavy = "\u{1}".repeat(crate::services::feedback::MAX_MESSAGE_CHARS);
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "message": escape_heavy }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn post_feedback_db_failure_returns_500_not_fake_success() {
        // Deliberate divergence from fire-and-forget events: validation
        // passes, then the insert fails against the dead pool — the client
        // must get an honest 500, never a fake success.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(dead_pool()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/feedback")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "message": "great game!" }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["error"], "internal error");
    }
}
