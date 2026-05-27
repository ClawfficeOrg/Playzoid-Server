//! `/players` HTTP endpoints.
//!
//! `GET    /players/{id}` — fetch a player's public profile (auth required).
//! `PUT    /players/{id}` — update own profile (auth required; own account only).
//! `DELETE /players/{id}` — soft-delete own account (auth required; own account only).
//! `POST   /players`      — placeholder; subaccount creation is implemented in task 0.2-8.

use crate::middleware::auth::AuthenticatedUser;
use crate::services::players::{self as players_svc, PlayerServiceError, UpdatePlayerInput};
use actix_web::{HttpResponse, web};
use serde::Deserialize;
use sqlx::MySqlPool;
use validator::Validate;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/players")
            .route("/{id}", web::get().to(get_player))
            .route("/{id}", web::put().to(update_player))
            .route("/{id}", web::delete().to(delete_player))
            .route("", web::post().to(create_player)),
    );
}

/// Fetch a player's public profile by `public_id`.
///
/// Any authenticated user may retrieve any player's public profile.
/// Returns 404 when the player does not exist or has been soft-deleted.
#[tracing::instrument(skip(pool, _user))]
async fn get_player(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    _user: AuthenticatedUser,
) -> HttpResponse {
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };
    let public_id = path.into_inner();
    match players_svc::find_player_view(pool.get_ref(), &public_id).await {
        Ok(Some(view)) => HttpResponse::Ok().json(view),
        Ok(None) => HttpResponse::NotFound().json(error_body("player not found")),
        Err(e) => {
            tracing::error!(error = ?e, "get_player failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

/// Fields that may be updated on a player profile.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct UpdatePlayerRequest {
    /// New username. Omit to leave unchanged.
    #[validate(length(min = 3, max = 64))]
    pub username: Option<String>,
    /// New email address. Omit to leave unchanged.
    #[validate(email)]
    pub email: Option<String>,
}

/// Update the authenticated player's own profile.
///
/// Returns 400 for validation errors, 403 when attempting to modify another
/// player's account, 404 when the player is not found, 409 on username/email
/// conflict.
#[tracing::instrument(skip(pool, user))]
async fn update_player(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    user: AuthenticatedUser,
    body: web::Json<UpdatePlayerRequest>,
) -> HttpResponse {
    if let Err(e) = body.validate() {
        return HttpResponse::BadRequest().json(error_body(&format!("validation: {e}")));
    }
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };
    let public_id = path.into_inner();
    let input = UpdatePlayerInput {
        username: body.username.clone(),
        email: body.email.clone(),
    };
    match players_svc::update_player(pool.get_ref(), &public_id, &user.player_public_id, input)
        .await
    {
        Ok(player) => HttpResponse::Ok().json(crate::entities::player::PlayerView::from(&player)),
        Err(PlayerServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("player not found"))
        }
        Err(PlayerServiceError::Forbidden) => {
            HttpResponse::Forbidden().json(error_body("you may only modify your own account"))
        }
        Err(PlayerServiceError::Duplicate) => {
            HttpResponse::Conflict().json(error_body("username or email already taken"))
        }
        Err(PlayerServiceError::Invalid(msg)) => HttpResponse::BadRequest().json(error_body(&msg)),
        Err(e) => {
            tracing::error!(error = ?e, "update_player failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

/// Soft-delete the authenticated player's own account.
///
/// Sets `status = 'deleted'` — the row is retained for FK integrity and will
/// no longer appear in any service query. Returns 204 on success.
#[tracing::instrument(skip(pool, user))]
async fn delete_player(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    user: AuthenticatedUser,
) -> HttpResponse {
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };
    let public_id = path.into_inner();
    match players_svc::soft_delete_player(pool.get_ref(), &public_id, &user.player_public_id).await
    {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(PlayerServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("player not found"))
        }
        Err(PlayerServiceError::Forbidden) => {
            HttpResponse::Forbidden().json(error_body("you may only delete your own account"))
        }
        Err(e) => {
            tracing::error!(error = ?e, "delete_player failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

/// Placeholder — subaccount creation is implemented in task 0.2-8.
async fn create_player() -> HttpResponse {
    HttpResponse::NotImplemented().json(error_body("subaccount creation not yet implemented"))
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

    // ── GET /players/{id} ──────────────────────────────────────────────────

    #[actix_web::test]
    async fn get_player_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/players/some-id")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn get_player_without_pool_returns_503() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/players/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── PUT /players/{id} ──────────────────────────────────────────────────

    #[actix_web::test]
    async fn update_player_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/players/some-id")
            .set_json(serde_json::json!({"username": "newname"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn update_player_rejects_short_username() {
        // Validation fires before pool check, so no pool needed for a 400.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/players/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"username": "ab"})) // < 3 chars
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn update_player_rejects_invalid_email() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/players/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"email": "not-an-email"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn update_player_without_pool_returns_503() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/players/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"username": "validname"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── DELETE /players/{id} ───────────────────────────────────────────────

    #[actix_web::test]
    async fn delete_player_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::delete()
            .uri("/players/some-id")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn delete_player_without_pool_returns_503() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::delete()
            .uri("/players/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
