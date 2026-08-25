//! `/leaderboards` HTTP endpoints.
//!
//! `GET  /leaderboards/{game_id}`                       — paginated top scores (auth required).
//! `POST /leaderboards/{game_id}/entries`               — submit a score (auth required).
//! `PUT  /leaderboards/{game_id}/entries/{player_id}`   — update own score (auth required).
//! `game_id` is the leaderboard's route identifier (`internal_name`).

use crate::middleware::auth::AuthenticatedUser;
use crate::services::leaderboards::{
    self as leaderboards_svc, LeaderboardServiceError, MAX_PER_PAGE, MAX_PROPS_BYTES,
};
use actix_web::{HttpResponse, web};
use serde::Deserialize;
use sqlx::MySqlPool;

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/leaderboards")
            .route("/{game_id}/entries", web::post().to(submit_entry))
            .route(
                "/{game_id}/entries/{player_id}",
                web::put().to(update_entry),
            )
            .route("/{game_id}", web::get().to(get_leaderboard)),
    );
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

// ── POST /leaderboards/{game_id}/entries ──────────────────────────────────────

/// Request body for submitting a leaderboard score.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitScoreRequest {
    /// The submitted score.
    pub score: i64,
    /// Optional game-specific props (JSON array).
    #[serde(default)]
    pub props: Option<serde_json::Value>,
}

/// Submit a score for the authenticated player on a leaderboard.
///
/// The owning player is taken from the JWT — callers cannot submit on behalf
/// of another player. One entry per player per leaderboard; re-submission
/// returns 409 (use the PUT update endpoint). Returns 201 with the stored
/// entry including its computed rank.
///
/// Returns 400 for invalid bodies, 404 for unknown leaderboards.
#[tracing::instrument(skip(pool, user))]
async fn submit_entry(
    path: web::Path<String>,
    pool: Option<web::Data<MySqlPool>>,
    user: AuthenticatedUser,
    body: web::Json<SubmitScoreRequest>,
) -> HttpResponse {
    // Validate props before the pool check so 400 wins over 503.
    if let Some(props) = body.props.as_ref()
        && (!props.is_array()
            || serde_json::to_string(props)
                .map(|s| s.len() > MAX_PROPS_BYTES)
                .unwrap_or(true))
    {
        return HttpResponse::BadRequest().json(error_body(
            "props must be a JSON array within the size limit",
        ));
    }
    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };
    let game_id = path.into_inner();

    match leaderboards_svc::submit_entry(
        pool.get_ref(),
        &game_id,
        &user.player_public_id,
        body.score,
        body.props.clone(),
    )
    .await
    {
        Ok(view) => {
            let mut resp = serde_json::json!({
                "playerId": view.player_id,
                "score": view.score,
                "rank": view.rank,
            });
            if let Some(props) = body.props.as_ref() {
                resp["props"] = props.clone();
            }
            HttpResponse::Created().json(resp)
        }
        Err(LeaderboardServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("leaderboard not found"))
        }
        Err(LeaderboardServiceError::PlayerNotFound) => {
            HttpResponse::NotFound().json(error_body("player not found"))
        }
        Err(LeaderboardServiceError::Duplicate) => HttpResponse::Conflict().json(error_body(
            "an entry for this player already exists on this leaderboard",
        )),
        Err(LeaderboardServiceError::Invalid(msg)) => {
            HttpResponse::BadRequest().json(error_body(&msg))
        }
        Err(e) => {
            tracing::error!(error = ?e, "submit_entry failed");
            HttpResponse::InternalServerError().json(error_body("internal error"))
        }
    }
}

// ── PUT /leaderboards/{game_id}/entries/{player_id} ───────────────────────────

/// Update the authenticated player's own score on a leaderboard.
///
/// The `{player_id}` path segment must match the JWT identity — cross-player
/// updates return 403. The entry must already exist (404 otherwise; use the
/// POST endpoint to create one). Omitted `props` keep their current value.
///
/// Returns 200 with the updated entry including its recomputed rank, 400 for
/// invalid bodies, 404 for unknown leaderboards or missing entries, 403 for
/// cross-player attempts.
#[tracing::instrument(skip(pool, user))]
async fn update_entry(
    path: web::Path<(String, String)>,
    pool: Option<web::Data<MySqlPool>>,
    user: AuthenticatedUser,
    body: web::Json<SubmitScoreRequest>,
) -> HttpResponse {
    // Validate props before the pool check so 400 wins over 503.
    if let Some(props) = body.props.as_ref()
        && (!props.is_array()
            || serde_json::to_string(props)
                .map(|s| s.len() > MAX_PROPS_BYTES)
                .unwrap_or(true))
    {
        return HttpResponse::BadRequest().json(error_body(
            "props must be a JSON array within the size limit",
        ));
    }
    let (game_id, player_id) = path.into_inner();

    // Ownership check before any DB access so cross-player requests fail fast.
    if player_id != user.player_public_id {
        return HttpResponse::Forbidden().json(error_body(
            "you may only update your own leaderboard entries",
        ));
    }

    let Some(pool) = pool else {
        return HttpResponse::ServiceUnavailable().json(error_body("database unavailable"));
    };

    match leaderboards_svc::update_entry(
        pool.get_ref(),
        &game_id,
        &player_id,
        &user.player_public_id,
        body.score,
        body.props.clone(),
    )
    .await
    {
        Ok(view) => {
            let mut resp = serde_json::json!({
                "playerId": view.player_id,
                "score": view.score,
                "rank": view.rank,
            });
            if let Some(props) = body.props.as_ref() {
                resp["props"] = props.clone();
            }
            HttpResponse::Ok().json(resp)
        }
        Err(LeaderboardServiceError::Forbidden) => HttpResponse::Forbidden().json(error_body(
            "you may only update your own leaderboard entries",
        )),
        Err(LeaderboardServiceError::EntryNotFound) => HttpResponse::NotFound()
            .json(error_body("no entry for this player on this leaderboard")),
        Err(LeaderboardServiceError::NotFound) => {
            HttpResponse::NotFound().json(error_body("leaderboard not found"))
        }
        Err(LeaderboardServiceError::PlayerNotFound) => {
            HttpResponse::NotFound().json(error_body("player not found"))
        }
        Err(LeaderboardServiceError::Invalid(msg)) => {
            HttpResponse::BadRequest().json(error_body(&msg))
        }
        Err(e) => {
            tracing::error!(error = ?e, "update_entry failed");
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

    // ── POST /leaderboards/{game_id}/entries ───────────────────────────────

    #[actix_web::test]
    async fn submit_entry_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/leaderboards/game-highscores/entries")
            .set_json(serde_json::json!({"score": 100}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn submit_entry_without_pool_returns_503() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/leaderboards/game-highscores/entries")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"score": 100}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn submit_entry_rejects_unknown_fields() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/leaderboards/game-highscores/entries")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"score": 100, "bogus": true}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn submit_entry_rejects_non_array_props() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/leaderboards/game-highscores/entries")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"score": 100, "props": {"not": "array"}}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ── PUT /leaderboards/{game_id}/entries/{player_id} ────────────────────

    #[actix_web::test]
    async fn update_entry_requires_auth() {
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/leaderboards/game-highscores/entries/player-uuid-1")
            .set_json(serde_json::json!({"score": 200}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn update_entry_without_pool_returns_503() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/leaderboards/game-highscores/entries/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"score": 200}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn update_entry_cross_player_returns_403() {
        // Ownership check runs before any SQL — fake pool never connects.
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/leaderboards/game-highscores/entries/player-uuid-2")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"score": 200}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn update_entry_rejects_non_array_props() {
        let token = valid_token("player-uuid-1");
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(stub_config()))
                .configure(config),
        )
        .await;
        let req = test::TestRequest::put()
            .uri("/leaderboards/game-highscores/entries/player-uuid-1")
            .insert_header(("Authorization", format!("Bearer {token}")))
            .set_json(serde_json::json!({"score": 200, "props": {"not": "array"}}))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
