//! `/leaderboards` HTTP endpoints.
//!
//! `GET /leaderboards/{game_id}` — paginated top scores (auth required).
//! `game_id` is the leaderboard's route identifier (`internal_name`).

use crate::middleware::auth::AuthenticatedUser;
use crate::services::leaderboards::{
    self as leaderboards_svc, LeaderboardServiceError, MAX_PER_PAGE,
};
use actix_web::{HttpResponse, web};
use serde::Deserialize;
use sqlx::MySqlPool;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/leaderboards").route("/{game_id}", web::get().to(get_leaderboard)));
}

/// Pagination query parameters for `GET /leaderboards/{game_id}`.
#[derive(Debug, Deserialize)]
pub struct PageParams {
    /// 1-based page number. Defaults to 1.
    page: Option<u64>,
    /// Entries per page, capped at [`MAX_PER_PAGE`]. Defaults to 50.
    per_page: Option<u64>,
}

/// Fetch one page of ranked top scores for a leaderboard.
///
/// Returns 400 for invalid pagination, 404 when the leaderboard is unknown.
#[tracing::instrument(skip(pool, _user))]
async fn get_leaderboard(
    path: web::Path<String>,
    query: web::Query<PageParams>,
    pool: Option<web::Data<MySqlPool>>,
    _user: AuthenticatedUser,
) -> HttpResponse {
    let game_id = path.into_inner();
    let page = query.page.unwrap_or(1);
    let per_page = query.per_page.unwrap_or(leaderboards_svc::DEFAULT_PER_PAGE);

    if page == 0 {
        return HttpResponse::BadRequest().json(error_body("page must be >= 1"));
    }
    if per_page == 0 || per_page > MAX_PER_PAGE {
        return HttpResponse::BadRequest().json(error_body(&format!(
            "per_page must be between 1 and {MAX_PER_PAGE}"
        )));
    }

    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };

    match leaderboards_svc::top_entries(pool.get_ref(), &game_id, page, per_page).await {
        Ok(resp) => HttpResponse::Ok().json(resp),
        Err(LeaderboardServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("leaderboard not found"))
        }
        Err(LeaderboardServiceError::Invalid(msg)) => {
            HttpResponse::BadRequest().json(error_body(&msg))
        }
        Err(e) => {
            tracing::error!(error = ?e, "get_leaderboard failed");
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
    use actix_web::{App, http::StatusCode, test};

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
    async fn get_leaderboard_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/leaderboards/game-highscores")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn get_leaderboard_without_pool_returns_503() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/leaderboards/game-highscores")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn get_leaderboard_rejects_zero_page() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/leaderboards/game-highscores?page=0")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn get_leaderboard_rejects_zero_per_page() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/leaderboards/game-highscores?per_page=0")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn get_leaderboard_rejects_oversized_per_page() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::get()
            .uri("/leaderboards/game-highscores?per_page=101")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
