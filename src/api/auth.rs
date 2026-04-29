use actix_web::{web, HttpResponse};

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/auth").route("/login", web::post().to(login)));
}

async fn login() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({"token": "<stub>", "expiry": 3600}))
}
