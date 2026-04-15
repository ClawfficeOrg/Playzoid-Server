use actix_web::{web, App, HttpServer};

mod api;
mod sockets;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();
    let bind = "127.0.0.1:8080";
    println!("Starting server on {}", bind);
    HttpServer::new(|| {
        App::new()
            .configure(api::auth::config)
            .configure(api::players::config)
            .configure(api::socket_ticket::config)
            .route("/ws", web::get().to(sockets::ws::ws_index))
    })
    .bind(bind)?
    .run()
    .await
}
