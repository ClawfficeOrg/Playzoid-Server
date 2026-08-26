//! `/saves` HTTP endpoints.
//!
//! `GET /saves/{player_id}` — list the authenticated player's game saves, newest
//! first (auth required). Saves are private per-player game state — unlike
//! profile reads this endpoint only ever returns the caller's own saves.

use crate::middleware::auth::AuthenticatedUser;
use crate::services::saves::{self as saves_svc, SaveServiceError};
use actix_web::{HttpResponse, web};
use sqlx::MySqlPool;

/// Register the `/saves` HTTP routes.
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/saves").route("/{player_id}", web::get().to(list_saves)));
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

    #[actix_web::test]
    async fn list_saves_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/saves/player-uuid-1")
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
            .uri("/saves/player-uuid-2")
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
            .uri("/saves/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
