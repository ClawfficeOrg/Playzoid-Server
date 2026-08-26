//! Integration tests for the WebSocket `/ws` endpoint.
//!
//! The WS handshake itself requires no database or Redis, so these run without
//! the Docker dev stack. In-process message routing is covered by the unit
//! tests in `src/sockets/ws.rs`.

use actix_web::{App, http::header, test, web};
use playzoid_server::sockets;

#[actix_web::test]
async fn ws_handshake_upgrades() {
    let app =
        test::init_service(App::new().route("/ws", web::get().to(sockets::ws::ws_index))).await;

    let req = test::TestRequest::get()
        .uri("/ws")
        .insert_header((header::CONNECTION, "upgrade"))
        .insert_header((header::UPGRADE, "websocket"))
        .insert_header(("sec-websocket-version", "13"))
        .insert_header(("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="))
        .to_request();

    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 101);
    assert!(resp.headers().contains_key("sec-websocket-accept"));
}

#[actix_web::test]
async fn ws_rejects_non_get() {
    let app =
        test::init_service(App::new().route("/ws", web::get().to(sockets::ws::ws_index))).await;

    let req = test::TestRequest::post().uri("/ws").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 404);
}
