//! `/v1/players` HTTP endpoints.
//!
//! `GET    /v1/players/{id}`             — fetch a player's public profile (auth required).
//! `PUT    /v1/players/{id}`             — update own profile (auth required; own account only).
//! `DELETE /v1/players/{id}`             — soft-delete own account (auth required; own account only).
//! `GET    /v1/players/{id}/subaccounts` — list own subaccounts (auth required; own account only).
//! `POST   /v1/players/subaccount`       — create a subaccount under the authenticated player.
//!
//! The routes are also mounted under the legacy unprefixed `/players` alias
//! for the 0.4.1 transition (upstream parity: canonical paths carry the
//! `/v1` prefix). Both mounts share one route definition so they cannot drift.

use crate::entities::player::PlayerView;
use crate::middleware::auth::AuthenticatedUser;
use crate::services::cache as cache_svc;
use crate::services::players::{
    self as players_svc, NewPlayer, PlayerServiceError, UpdatePlayerInput,
};
use actix_web::{HttpResponse, Scope, web};
use redis::aio::ConnectionManager;
use serde::Deserialize;
use sqlx::MySqlPool;
use validator::Validate;

/// Register the player routes: canonical `/v1/players` plus the legacy
/// `/players` alias kept during the 0.4.1 transition.
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(scoped("/v1/players"))
        .service(scoped("/players"));
}

/// Build the player routes under the given scope prefix so the canonical and
/// legacy mounts share a single route definition.
fn scoped(prefix: &str) -> Scope {
    web::scope(prefix)
        // static-segment routes must come before /{id} or actix will
        // match "subaccount" as an id value
        .route("/subaccount", web::post().to(create_subaccount))
        .route("/{id}/subaccounts", web::get().to(list_subaccounts))
        .route("/{id}", web::get().to(get_player))
        .route("/{id}", web::put().to(update_player))
        .route("/{id}", web::delete().to(delete_player))
}

// ── GET /v1/players/{id} ─────────────────────────────────────────────────────────

/// Fetch a player's public profile by `public_id`.
///
/// Checks the Redis cache first; falls back to the database on a miss.
/// Any authenticated user may retrieve any player's public profile.
#[tracing::instrument(skip(pool, cache, cfg, _user))]
async fn get_player(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    cache: Option<web::Data<ConnectionManager>>,
    cfg: web::Data<crate::config::Config>,
    _user: AuthenticatedUser,
) -> HttpResponse {
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };
    let public_id = path.into_inner();

    // Cache read — treat any error as a miss.
    if let Some(ref c) = cache {
        match cache_svc::get_player_view::<PlayerView>(c.get_ref().clone(), &public_id).await {
            Ok(Some(view)) => return HttpResponse::Ok().json(view),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "cache miss (read error)"),
        }
    }

    match players_svc::find_player_view(pool.get_ref(), &public_id).await {
        Ok(Some(view)) => {
            // Populate cache in the background — ignore errors.
            if let Some(ref c) = cache
                && let Err(e) = cache_svc::set_player_view(
                    c.get_ref().clone(),
                    &public_id,
                    &view,
                    cfg.jwt_expiry_secs,
                )
                .await
            {
                tracing::warn!(error = %e, "cache set failed");
            }
            HttpResponse::Ok().json(view)
        }
        Ok(None) => HttpResponse::NotFound().json(error_body("player not found")),
        Err(e) => {
            tracing::error!(error = ?e, "get_player failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

// ── PUT /v1/players/{id} ─────────────────────────────────────────────────────────

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
/// player's account, 404 when the player is not found, 409 on conflict.
/// Invalidates the Redis cache entry on success.
#[tracing::instrument(skip(pool, cache, user))]
async fn update_player(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    cache: Option<web::Data<ConnectionManager>>,
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
        Ok(player) => {
            if let Some(ref c) = cache
                && let Err(e) = cache_svc::invalidate_player(c.get_ref().clone(), &public_id).await
            {
                tracing::warn!(error = %e, "cache invalidation failed after update");
            }
            HttpResponse::Ok().json(PlayerView::from(&player))
        }
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

// ── DELETE /v1/players/{id} ──────────────────────────────────────────────────────

/// Soft-delete the authenticated player's own account.
///
/// Sets `status = 'deleted'` — the row is retained for FK integrity. Returns
/// 204 on success. Invalidates the Redis cache entry on success.
#[tracing::instrument(skip(pool, cache, user))]
async fn delete_player(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    cache: Option<web::Data<ConnectionManager>>,
    user: AuthenticatedUser,
) -> HttpResponse {
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };
    let public_id = path.into_inner();
    match players_svc::soft_delete_player(pool.get_ref(), &public_id, &user.player_public_id).await
    {
        Ok(()) => {
            if let Some(ref c) = cache
                && let Err(e) = cache_svc::invalidate_player(c.get_ref().clone(), &public_id).await
            {
                tracing::warn!(error = %e, "cache invalidation failed after delete");
            }
            HttpResponse::NoContent().finish()
        }
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

// ── POST /v1/players/subaccount ──────────────────────────────────────────────────

/// Request body for creating a subaccount under the authenticated player.
#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct CreateSubaccountRequest {
    /// Username for the new subaccount.
    #[validate(length(min = 3, max = 64))]
    pub username: String,
    /// Optional email for the new subaccount.
    #[validate(email)]
    pub email: Option<String>,
    /// Password for the new subaccount.
    #[validate(length(min = 8, max = 1024))]
    pub password: String,
}

/// Create a new subaccount linked to the authenticated player as its parent.
///
/// The parent relationship is inferred from the JWT — callers cannot specify
/// an arbitrary parent. Returns 201 with the new subaccount's [`PlayerView`].
#[tracing::instrument(skip(pool, user))]
async fn create_subaccount(
    pool: Option<web::Data<MySqlPool>>,
    user: AuthenticatedUser,
    body: web::Json<CreateSubaccountRequest>,
) -> HttpResponse {
    if let Err(e) = body.validate() {
        return HttpResponse::BadRequest().json(error_body(&format!("validation: {e}")));
    }
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };
    let input = NewPlayer {
        username: &body.username,
        email: body.email.as_deref(),
        password_plain: &body.password,
        parent_account_public_id: Some(&user.player_public_id),
    };
    match players_svc::create_player(pool.get_ref(), input).await {
        Ok(p) => {
            let mut view = PlayerView::from(&p);
            view.parent_account_id = Some(user.player_public_id.clone());
            HttpResponse::Created().json(view)
        }
        Err(PlayerServiceError::Duplicate) => {
            HttpResponse::Conflict().json(error_body("username or email already taken"))
        }
        Err(PlayerServiceError::Invalid(msg)) => HttpResponse::BadRequest().json(error_body(&msg)),
        Err(e) => {
            tracing::error!(error = ?e, "create_subaccount failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

// ── GET /v1/players/{id}/subaccounts ─────────────────────────────────────────────

/// List all non-deleted subaccounts for the given parent player.
///
/// Only the authenticated player may list their own subaccounts (403 for
/// cross-account requests). Returns an empty array when no subaccounts exist.
#[tracing::instrument(skip(pool, user))]
async fn list_subaccounts(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    user: AuthenticatedUser,
) -> HttpResponse {
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };
    let parent_id = path.into_inner();
    match players_svc::find_subaccounts(pool.get_ref(), &parent_id, &user.player_public_id).await {
        Ok(views) => HttpResponse::Ok().json(views),
        Err(PlayerServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("player not found"))
        }
        Err(PlayerServiceError::Forbidden) => {
            HttpResponse::Forbidden().json(error_body("you may only view your own subaccounts"))
        }
        Err(e) => {
            tracing::error!(error = ?e, "list_subaccounts failed");
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

    // ── GET /v1/players/{id} ──────────────────────────────────────────────────

    #[actix_web::test]
    async fn get_player_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/players/some-id")
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
            .uri("/v1/players/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── PUT /v1/players/{id} ──────────────────────────────────────────────────

    #[actix_web::test]
    async fn update_player_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/v1/players/some-id")
            .set_json(serde_json::json!({"username": "newname"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn update_player_rejects_short_username() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/v1/players/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"username": "ab"}))
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
            .uri("/v1/players/player-uuid-1")
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
            .uri("/v1/players/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"username": "validname"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── DELETE /v1/players/{id} ───────────────────────────────────────────────

    #[actix_web::test]
    async fn delete_player_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::delete()
            .uri("/v1/players/some-id")
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
            .uri("/v1/players/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── POST /v1/players/subaccount ───────────────────────────────────────────

    #[actix_web::test]
    async fn create_subaccount_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/players/subaccount")
            .set_json(serde_json::json!({"username": "child", "password": "pass12345"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn create_subaccount_rejects_short_password() {
        let token = valid_token("parent-uuid");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/players/subaccount")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"username": "child", "password": "short"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn create_subaccount_without_pool_returns_503() {
        let token = valid_token("parent-uuid");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/players/subaccount")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"username": "child", "password": "pass12345678"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── GET /v1/players/{id}/subaccounts ──────────────────────────────────────

    #[actix_web::test]
    async fn list_subaccounts_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/players/parent-uuid/subaccounts")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn list_subaccounts_without_pool_returns_503() {
        let token = valid_token("parent-uuid");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/players/parent-uuid/subaccounts")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Legacy `/players` alias mount (0.4.1 transition) ───────────────────

    /// The unprefixed alias must keep routing identically during transition.
    #[actix_web::test]
    async fn get_player_legacy_alias_requires_auth() {
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
    async fn create_subaccount_legacy_alias_rejects_short_password() {
        let token = valid_token("parent-uuid");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/players/subaccount")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"username": "child", "password": "short"}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
