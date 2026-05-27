//! `/players` HTTP endpoints.
//!
//! Currently a guarded stub that verifies auth middleware plumbing.
//! Full CRUD (GET/PUT/DELETE) is implemented in tasks 0.2-4 through 0.2-6.

use crate::middleware::auth::AuthenticatedUser;
use actix_web::{HttpResponse, web};

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/players")
            .route("/{id}", web::get().to(get_player))
            .route("", web::post().to(create_player)),
    );
}

/// Placeholder — returns the authenticated caller's own id.
/// Will be replaced with a real DB lookup in task 0.2-4.
#[tracing::instrument(skip(user))]
async fn get_player(path: web::Path<String>, user: AuthenticatedUser) -> HttpResponse {
    let id = path.into_inner();
    HttpResponse::Ok().json(serde_json::json!({
        "id": id,
        "requested_by": user.player_public_id,
    }))
}

/// Placeholder — will be replaced with subaccount creation in task 0.2-8.
async fn create_player() -> HttpResponse {
    HttpResponse::Created().json(serde_json::json!({"id": "player124", "status": "created"}))
}
