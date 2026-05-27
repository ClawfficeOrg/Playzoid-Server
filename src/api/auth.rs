//! `/auth` HTTP endpoints — register and login.
//!
//! `POST /auth/register` — create a new player; returns the public projection.
//! `POST /auth/login`    — verify credentials; returns a signed JWT.
//!
//! Both endpoints require the DB pool injected via `web::Data<MySqlPool>`.
//! When the pool is absent (degraded startup), they return `503`.

use crate::config::Config;
use crate::entities::player::PlayerView;
use crate::services::auth as auth_svc;
use crate::services::players::{self as players_svc, NewPlayer, PlayerServiceError};
use actix_web::{HttpResponse, web};
use serde::{Deserialize, Serialize};
use sqlx::MySqlPool;
use validator::Validate;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/auth")
            .route("/register", web::post().to(register))
            .route("/login", web::post().to(login)),
    );
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
                Ok(token) => HttpResponse::Ok().json(LoginResponse {
                    token,
                    expires_in: cfg.jwt_expiry_secs,
                    player: PlayerView::from(&p),
                }),
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
    use actix_web::{App, http::StatusCode, test};

    /// Ensures that without an injected pool the endpoints return 503 and
    /// not 500 — we degrade gracefully rather than panic on missing state.
    #[actix_web::test]
    async fn register_returns_503_without_pool() {
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
