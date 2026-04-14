use actix_web::{web, HttpResponse};
use serde::Deserialize;
use crate::sockets::tickets;

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct CreateTicketRequest {
    pub alias_id: i64,
}

#[allow(dead_code)]
pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/v1/socket-tickets").route("", web::post().to(create_ticket)));
}

#[allow(dead_code)]
async fn create_ticket(body: web::Json<CreateTicketRequest>) -> HttpResponse {
    let ticket = tickets::create_ticket(body.alias_id);
    HttpResponse::Ok().json(serde_json::json!({"ticket": ticket}))
}
