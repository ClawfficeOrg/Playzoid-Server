use crate::sockets::tickets;
use actix_web::{HttpResponse, web};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CreateTicketRequest {
    pub alias_id: i64,
}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/v1/socket-tickets").route("", web::post().to(create_ticket)));
}

async fn create_ticket(body: web::Json<CreateTicketRequest>) -> HttpResponse {
    let ticket = tickets::create_ticket(body.alias_id);
    HttpResponse::Ok().json(serde_json::json!({"ticket": ticket}))
}
