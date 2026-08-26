//! `/v1/games/{game_id}/settings` HTTP endpoints.
//!
//! `GET /v1/games/{game_id}/settings` — read one game's stored JSON config
//!                                       (auth required; unknown game → 404).
//! `PUT /v1/games/{game_id}/settings`   — create-or-replace one game's JSON
//!                                       config, upsert keyed on the opaque
//!                                       route identifier (auth required;
//!                                       returns the stored view).
//!
//! Both endpoints are auth-guarded via [`AuthenticatedUser`]; any valid JWT
//! may read or write (v0 trade-off — no `games` table / ownership scopes
//! exist yet, see `docs/memory.md`). Config is validated server-side: not
//! JSON `null`, serialized size ≤ `MAX_CONFIG_BYTES` (32 KiB).
//!
//! Unlike the Phase 0.2/0.3 routes there is **no legacy alias mount**: this
//! route is new after the 0.4.1 prefix-parity pass, so only the canonical
//! upstream-style `/v1` spelling exists (same precedent as
//! `/v1/socket-tickets`).

use crate::middleware::auth::AuthenticatedUser;
use crate::services::game_settings::{self as settings_svc, GameSettingsServiceError};
use actix_web::{HttpResponse, Scope, web};
use serde::Deserialize;
use sqlx::MySqlPool;

/// Register the game-settings routes under their canonical `/v1/games` scope.
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(scoped("/v1/games"));
}

/// Build the game-settings routes under the given scope prefix.
fn scoped(prefix: &str) -> Scope {
    web::scope(prefix)
        .route("/{game_id}/settings", web::get().to(get_settings))
        .route("/{game_id}/settings", web::put().to(put_settings))
}

/// Request body for storing a game's configuration.
///
/// The wrapper object exists so unknown top-level fields are rejected at
/// deserialization time (validator precedent) while the inner `config`
/// itself stays arbitrary JSON — any shape, validated only for null-ness
/// and serialized size by the service layer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutSettingsRequest {
    /// Arbitrary per-game configuration JSON (must not be `null`).
    pub config: serde_json::Value,
}

/// Read one game's stored configuration.
///
/// Unknown games return 404. Returns 200 with the stored view
/// (`gameId`, `config`, `createdAt`, `updatedAt`).
#[tracing::instrument(skip(pool, _user))]
async fn get_settings(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    _user: AuthenticatedUser,
) -> HttpResponse {
    let game_id = path.into_inner();

    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };

    match settings_svc::get_settings(pool.get_ref(), &game_id).await {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(GameSettingsServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("game settings not found"))
        }
        Err(e) => {
            tracing::error!(error = ?e, "get_settings failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

/// Store one game's configuration (create-or-replace).
///
/// The write is an upsert keyed on the opaque `{game_id}` route identifier:
/// the first PUT creates the row (200 with the stored view), later PUTs
/// replace only the config. Invalid bodies return 400 before any SQL
/// (null config, oversized config, malformed id).
#[tracing::instrument(skip(pool, _user, body))]
async fn put_settings(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    _user: AuthenticatedUser,
    body: web::Json<PutSettingsRequest>,
) -> HttpResponse {
    let game_id = path.into_inner();

    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };

    match settings_svc::put_settings(pool.get_ref(), &game_id, &body.config).await {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(GameSettingsServiceError::Invalid(msg)) => {
            HttpResponse::BadRequest().json(error_body(&msg))
        }
        Err(e) => {
            tracing::error!(error = ?e, "put_settings failed");
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
    use crate::config::{Config, RateLimitConfig};
    use crate::services::auth as auth_svc;
    use actix_web::{App, http::StatusCode, test, web};

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

    #[actix_web::test]
    async fn get_settings_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/games/game-1/settings")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn put_settings_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/v1/games/game-1/settings")
            .set_json(serde_json::json!({ "config": { "difficulty": "hard" } }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn get_settings_without_pool_returns_503() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/games/game-1/settings")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn put_settings_without_pool_returns_503() {
        // Valid body; the pool check fires before the service call.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/v1/games/game-1/settings")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "config": { "difficulty": "hard" } }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn put_settings_rejects_missing_config_field() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/v1/games/game-1/settings")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "bogus": true }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn put_settings_rejects_unknown_top_level_fields() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/v1/games/game-1/settings")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({
                "config": { "difficulty": "hard" },
                "extra": 1
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn put_settings_oversized_returns_400_pre_sql() {
        // Service-level size validation fires before any SQL, so a lazy
        // (never-connecting) pool is safe to register here.
        let token = valid_token("player-uuid-1");
        let pool = sqlx::MySqlPool::connect_lazy("mysql://test:test@127.0.0.1/test")
            .expect("lazy pool creation should not fail");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(pool))
                .configure(config),
        )
        .await;
        let big = serde_json::json!({ "data": "x".repeat(settings_svc::MAX_CONFIG_BYTES) });
        let req = test::TestRequest::put()
            .uri("/v1/games/game-1/settings")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "config": big }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn put_settings_over_64_char_game_id_returns_400_pre_sql() {
        // Id-length validation also runs pre-SQL against a lazy pool.
        let token = valid_token("player-uuid-1");
        let pool = sqlx::MySqlPool::connect_lazy("mysql://test:test@127.0.0.1/test")
            .expect("lazy pool creation should not fail");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .app_data(web::Data::new(pool))
                .configure(config),
        )
        .await;
        let too_long = "g".repeat(65);
        let req = test::TestRequest::put()
            .uri(&format!("/v1/games/{too_long}/settings"))
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({ "config": { "a": 1 } }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
