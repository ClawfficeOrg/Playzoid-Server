//! `/v1/auth` HTTP endpoints — register and login.
//!
//! `POST /v1/auth/register` — create a new player; returns the public projection.
//! `POST /v1/auth/login`    — verify credentials; returns a signed JWT.
//!
//! The routes are also mounted under the legacy unprefixed `/auth` alias for
//! the 0.4.1 transition (upstream parity: canonical paths carry the `/v1`
//! prefix). Both mounts share one route definition so they cannot drift.
//!
//! Both endpoints require the DB pool injected via `web::Data<MySqlPool>`.
//! When the pool is absent (degraded startup), they return `503`.

use crate::config::Config;
use crate::entities::player::PlayerView;
use crate::services::auth as auth_svc;
use crate::services::cache as cache_svc;
use crate::services::players::{self as players_svc, NewPlayer, PlayerServiceError};
use actix_web::{HttpResponse, Scope, web};
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;
use validator::Validate;

/// Register the auth routes: canonical `/v1/auth` plus the legacy `/auth`
/// alias kept during the 0.4.1 transition.
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(scoped("/v1/auth")).service(scoped("/auth"));
}

/// Build the register/login routes under the given scope prefix so the
/// canonical and legacy mounts share a single route definition.
fn scoped(prefix: &str) -> Scope {
    web::scope(prefix)
        .route("/register", web::post().to(register))
        .route("/login", web::post().to(login))
}

/// Register payload. Username/password sizing mirrors the DB constraints
/// and `services::auth` plaintext-password limits.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct RegisterRequest {
    #[validate(length(min = 3, max = 64))]
    pub username: String,
    #[validate(email)]
    pub email: Option<String>,
    #[validate(length(min = 8, max = 1024))]
    pub password: String,
    /// Optional — set when registering a subaccount under an existing root.
    pub parent_account_id: Option<String>,
}

/// Login payload. Username + plaintext password.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    #[validate(length(min = 3, max = 64))]
    pub username: String,
    #[validate(length(min = 1, max = 1024))]
    pub password: String,
}

/// Successful login response: a JWT plus its TTL in seconds.
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_in: u64,
    pub player: PlayerView,
}

async fn register(
    pool: Option<web::Data<MySqlPool>>,
    body: web::Json<RegisterRequest>,
) -> HttpResponse {
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };
    if let Err(e) = body.validate() {
        return HttpResponse::BadRequest().json(error_body(&format!("validation: {e}")));
    }

    let input = NewPlayer {
        username: &body.username,
        email: body.email.as_deref(),
        password_plain: &body.password,
        parent_account_public_id: body.parent_account_id.as_deref(),
    };

    match players_svc::create_player(pool.get_ref(), input).await {
        Ok(p) => HttpResponse::Created().json(PlayerView::from(&p)),
        Err(PlayerServiceError::Duplicate) => {
            HttpResponse::Conflict().json(error_body("username or email already taken"))
        }
        Err(PlayerServiceError::Invalid(msg)) => HttpResponse::BadRequest().json(error_body(&msg)),
        Err(e) => {
            tracing::error!(error = ?e, "register failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

async fn login(
    pool: Option<web::Data<MySqlPool>>,
    cfg: web::Data<Config>,
    cache: Option<web::Data<ConnectionManager>>,
    body: web::Json<LoginRequest>,
) -> HttpResponse {
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };
    if let Err(e) = body.validate() {
        return HttpResponse::BadRequest().json(error_body(&format!("validation: {e}")));
    }

    match players_svc::verify_credentials(pool.get_ref(), &body.username, &body.password).await {
        Ok(Some(p)) => {
            match auth_svc::issue_jwt(&cfg.jwt_secret, &p.public_id, cfg.jwt_expiry_secs) {
                Ok(token) => {
                    let view = PlayerView::from(&p);
                    // Populate cache on successful login — best effort.
                    if let Some(ref c) = cache
                        && let Err(e) = cache_svc::set_player_view(
                            c.get_ref().clone(),
                            &p.public_id,
                            &view,
                            cfg.jwt_expiry_secs,
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "cache set failed on login");
                    }
                    HttpResponse::Ok().json(LoginResponse {
                        token,
                        expires_in: cfg.jwt_expiry_secs,
                        player: view,
                    })
                }
                Err(e) => {
                    tracing::error!(error = ?e, "issue_jwt failed");
                    HttpResponse::InternalServerError().json(error_body("internal error"))
                }
            }
        }
        Ok(None) => HttpResponse::Unauthorized().json(error_body("invalid credentials")),
        Err(e) => {
            tracing::error!(error = ?e, "verify_credentials failed");
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
    use crate::config::RateLimitConfig;
    use actix_web::{App, http::StatusCode, test};

    /// Ensures that without an injected pool the endpoints return 503 and
    /// not 500 — we degrade gracefully rather than panic on missing state.
    #[actix_web::test]
    async fn register_returns_503_without_pool() {
        let app = test::init_service(App::new().configure(config)).await;
        let req = test::TestRequest::post()
            .uri("/v1/auth/register")
            .set_json(serde_json::json!({
                "username": "alice",
                "password": "supersecret"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn login_returns_503_without_pool() {
        // login requires web::Data<Config> to issue a JWT; register a stub so
        // actix can extract it before the handler checks for the pool.
        let cfg = Config {
            host: "127.0.0.1".into(),
            port: 8080,
            database_url: "mysql://test".into(),
            redis_url: "redis://test".into(),
            jwt_secret: "a".repeat(Config::MIN_JWT_SECRET_LEN),
            jwt_expiry_secs: 3600,
            rate_limit: RateLimitConfig::default(),
        };
        let app =
            test::init_service(App::new().app_data(web::Data::new(cfg)).configure(config)).await;
        let req = test::TestRequest::post()
            .uri("/v1/auth/login")
            .set_json(serde_json::json!({"username":"alice","password":"hunter2hunter2"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Legacy `/auth` alias mount (0.4.1 transition) ──────────────────────

    /// The unprefixed alias must keep routing identically during transition.
    #[actix_web::test]
    async fn register_legacy_alias_returns_503_without_pool() {
        let app = test::init_service(App::new().configure(config)).await;
        let req = test::TestRequest::post()
            .uri("/auth/register")
            .set_json(serde_json::json!({
                "username": "alice",
                "password": "supersecret"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn login_legacy_alias_returns_503_without_pool() {
        let cfg = Config {
            host: "127.0.0.1".into(),
            port: 8080,
            database_url: "mysql://test".into(),
            redis_url: "redis://test".into(),
            jwt_secret: "a".repeat(Config::MIN_JWT_SECRET_LEN),
            jwt_expiry_secs: 3600,
            rate_limit: RateLimitConfig::default(),
        };
        let app =
            test::init_service(App::new().app_data(web::Data::new(cfg)).configure(config)).await;
        let req = test::TestRequest::post()
            .uri("/auth/login")
            .set_json(serde_json::json!({"username":"alice","password":"hunter2hunter2"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
