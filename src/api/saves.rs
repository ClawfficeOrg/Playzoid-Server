//! `/v1/saves` HTTP endpoints.
//!
//! `POST /v1/saves`                           — create a game save (auth required).
//! `GET  /v1/saves/{player_id}`               — list the authenticated player's game saves,
//!                                              newest first (auth required). Saves are
//!                                              private per-player game state — unlike
//!                                              profile reads these endpoints only ever
//!                                              touch the caller's own saves.
//! `GET  /v1/saves/{player_id}/{save_id}`     — retrieve a single save (auth required).
//! `DELETE /v1/saves/{player_id}/{save_id}`    — delete a single save (auth required).
//!
//! The routes are also mounted under the legacy unprefixed `/saves` alias for
//! the 0.4.1 transition (upstream parity: canonical paths carry the `/v1`
//! prefix). Both mounts share one route definition so they cannot drift.

use crate::middleware::auth::AuthenticatedUser;
use crate::services::saves::{self as saves_svc, SaveServiceError};
use actix_web::{HttpResponse, Scope, web};
use serde::Deserialize;
use sqlx::MySqlPool;
use validator::Validate;

/// Register the save routes: canonical `/v1/saves` plus the legacy `/saves`
/// alias kept during the 0.4.1 transition.
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(scoped("/v1/saves")).service(scoped("/saves"));
}

/// Build the save routes under the given scope prefix so the canonical and
/// legacy mounts share a single route definition.
fn scoped(prefix: &str) -> Scope {
    web::scope(prefix)
        .route("", web::post().to(create_save))
        .route("/{player_id}", web::get().to(list_saves))
        .route("/{player_id}/{save_id}", web::get().to(get_save))
        .route("/{player_id}/{save_id}", web::delete().to(delete_save))
}

/// Request body for creating a game save.
///
/// Mirrors the verified Talo `CreateSaveRequest` shape (`playerId` optional —
/// defaults to the JWT identity; a supplied value must match it).
#[derive(Debug, Deserialize, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSaveRequest {
    /// Human-readable save name, 1..=255 characters.
    #[validate(length(min = 1, max = 255))]
    pub name: String,
    /// Optional owning player — must equal the JWT identity when supplied.
    pub player_id: Option<String>,
    /// Arbitrary game-state JSON blob (must not be JSON `null`).
    pub save: serde_json::Value,
    /// Optional game-specific metadata.
    pub metadata: Option<serde_json::Value>,
}

/// Create a game save for the authenticated player.
///
/// `playerId` in the body is optional: absent, it defaults to the JWT
/// identity; present, it must match the JWT (403 otherwise). This preserves
/// the 0.3.6 own-only property while staying compatible with clients that
/// send the Talo-shaped `playerId`. Unknown or soft-deleted players return
/// 404; invalid bodies return 400. Returns 201 with the stored `SaveView`.
#[tracing::instrument(skip(pool, user))]
async fn create_save(
    pool: Option<web::Data<MySqlPool>>,
    user: AuthenticatedUser,
    body: web::Json<CreateSaveRequest>,
) -> HttpResponse {
    if let Err(e) = body.validate() {
        return HttpResponse::BadRequest().json(error_body(&format!("validation: {e}")));
    }

    // Ownership check before the pool check so cross-player requests fail fast.
    let owning_player = match body.player_id.as_deref() {
        Some(pid) if pid != user.player_public_id => {
            return HttpResponse::Forbidden()
                .json(error_body("you may only create your own saves"));
        }
        _ => user.player_public_id.clone(),
    };

    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };

    match saves_svc::create_save(
        pool.get_ref(),
        &owning_player,
        &body.name,
        &body.save,
        body.metadata.as_ref(),
    )
    .await
    {
        Ok(view) => HttpResponse::Created().json(view),
        Err(SaveServiceError::Invalid(msg)) => HttpResponse::BadRequest().json(error_body(&msg)),
        Err(SaveServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("player not found"))
        }
        Err(e) => {
            tracing::error!(error = ?e, "create_save failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

/// List all saves owned by the authenticated player, newest first.
///
/// The `{player_id}` path segment must match the JWT identity — cross-player
/// reads return 403 before any database work. Unknown or soft-deleted players
/// return 404; a player with no saves returns 200 with an empty array.
#[tracing::instrument(skip(pool, user))]
async fn list_saves(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    user: AuthenticatedUser,
) -> HttpResponse {
    let player_id = path.into_inner();

    // Ownership check before the pool check so cross-player requests fail fast.
    if player_id != user.player_public_id {
        return HttpResponse::Forbidden().json(error_body("you may only view your own saves"));
    }

    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };

    match saves_svc::list_saves(pool.get_ref(), &player_id).await {
        Ok(views) => HttpResponse::Ok().json(views),
        Err(SaveServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("player not found"))
        }
        Err(e) => {
            tracing::error!(error = ?e, "list_saves failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

/// Retrieve a single save owned by the authenticated player.
///
/// The `{player_id}` path segment must match the JWT identity — cross-player
/// requests return 403 before any database work. The `{save_id}` is selected
/// scoped to the owning player's internal id, so an unknown save id — or one
/// owned by a different player — returns 404 (never leaks). Unknown or
/// soft-deleted players return 404. Returns 200 with the stored `SaveView`.
#[tracing::instrument(skip(pool, user))]
async fn get_save(
    path: web::Path<(String, String)>,
    pool: Option<web::Data<MySqlPool>>,
    user: AuthenticatedUser,
) -> HttpResponse {
    let (player_id, save_id) = path.into_inner();

    // Ownership check before the pool check so cross-player requests fail fast.
    if player_id != user.player_public_id {
        return HttpResponse::Forbidden().json(error_body("you may only view your own saves"));
    }

    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };

    match saves_svc::get_save(pool.get_ref(), &player_id, &save_id).await {
        Ok(view) => HttpResponse::Ok().json(view),
        Err(SaveServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("player or save not found"))
        }
        Err(e) => {
            tracing::error!(error = ?e, "get_save failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

fn error_body(msg: &str) -> serde_json::Value {
    serde_json::json!({ "error": msg })
}

/// Delete a single save owned by the authenticated player.
///
/// The `{player_id}` path segment must match the JWT identity — cross-player
/// requests return 403 before any database work. The `{save_id}` is deleted
/// scoped to the owning player's internal id, so an unknown save id — or one
/// owned by a different player — returns 404 (never leaks). Unknown or
/// soft-deleted players return 404. Returns 204 No Content on success.
#[tracing::instrument(skip(pool, user))]
async fn delete_save(
    path: web::Path<(String, String)>,
    pool: Option<web::Data<MySqlPool>>,
    user: AuthenticatedUser,
) -> HttpResponse {
    let (player_id, save_id) = path.into_inner();

    // Ownership check before the pool check so cross-player requests fail fast.
    if player_id != user.player_public_id {
        return HttpResponse::Forbidden().json(error_body("you may only delete your own saves"));
    }

    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };

    match saves_svc::delete_save(pool.get_ref(), &player_id, &save_id).await {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(SaveServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("player or save not found"))
        }
        Err(e) => {
            tracing::error!(error = ?e, "delete_save failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
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
    async fn list_saves_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/saves/player-uuid-1")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn list_saves_cross_player_returns_403() {
        // Ownership check runs before the pool check — no pool is registered.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/saves/player-uuid-2")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn list_saves_without_pool_returns_503() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/saves/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── POST /v1/saves ────────────────────────────────────────────────────────

    #[actix_web::test]
    async fn create_save_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/saves")
            .set_json(serde_json::json!({
                "name": "slot-1",
                "save": { "hp": 100 }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn create_save_cross_player_returns_403() {
        // Ownership check runs before the pool check — no pool is registered.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/saves")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({
                "name": "slot-1",
                "playerId": "player-uuid-2",
                "save": { "hp": 100 }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn create_save_matching_player_id_passes_ownership() {
        // Matching playerId passes ownership, then hits the missing pool → 503.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/saves")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({
                "name": "slot-1",
                "playerId": "player-uuid-1",
                "save": { "hp": 100 }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn create_save_without_pool_returns_503() {
        // Omitted playerId defaults to the JWT identity and reaches the pool check.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/saves")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({
                "name": "slot-1",
                "save": { "hp": 100 }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn create_save_rejects_unknown_fields() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/saves")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({
                "name": "slot-1",
                "save": { "hp": 100 },
                "bogus": true
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn create_save_rejects_empty_name() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/v1/saves")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({
                "name": "",
                "save": { "hp": 100 }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn create_save_rejects_oversized_save() {
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
        let big = serde_json::json!({ "data": "x".repeat(saves_svc::MAX_SAVE_BYTES) });
        let req = test::TestRequest::post()
            .uri("/v1/saves")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({
                "name": "slot-1",
                "save": big
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ── GET /v1/saves/{player_id}/{save_id} ───────────────────────────────────

    #[actix_web::test]
    async fn get_save_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/saves/player-uuid-1/save-uuid-1")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn get_save_cross_player_returns_403() {
        // Ownership check runs before the pool check — no pool is registered.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/saves/player-uuid-2/save-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn get_save_without_pool_returns_503() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/v1/saves/player-uuid-1/save-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── DELETE /v1/saves/{player_id}/{save_id} ────────────────────────────────

    #[actix_web::test]
    async fn delete_save_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::delete()
            .uri("/v1/saves/player-uuid-1/save-uuid-1")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn delete_save_cross_player_returns_403() {
        // Ownership check runs before the pool check — no pool is registered.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::delete()
            .uri("/v1/saves/player-uuid-2/save-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn delete_save_without_pool_returns_503() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::delete()
            .uri("/v1/saves/player-uuid-1/save-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Legacy `/saves` alias mount (0.4.1 transition) ─────────────────────

    /// The unprefixed alias must keep routing identically during transition.
    #[actix_web::test]
    async fn create_save_legacy_alias_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/saves")
            .set_json(serde_json::json!({
                "name": "slot-1",
                "save": { "hp": 100 }
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn list_saves_legacy_alias_cross_player_returns_403() {
        // Ownership check runs before the pool check — no pool is registered.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/saves/player-uuid-2")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
