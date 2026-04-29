use actix_web::{web, HttpResponse};

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/players").route("/{id}", web::get().to(get_player)).route("", web::post().to(create_player)));
}

async fn get_player(path: web::Path<String>) -> HttpResponse {
    let id = path.into_inner();
    HttpResponse::Ok().json(serde_json::json!({"id": id, "username": "sample_user", "status": "online"}))
}

async fn create_player() -> HttpResponse {
    HttpResponse::Created().json(serde_json::json!({"id": "player124", "status": "created"}))
}
