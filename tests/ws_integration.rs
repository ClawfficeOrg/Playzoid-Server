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
async fn ws_handshake_upgrades_without_db_pool() {
    // Regression guard for the `Option<web::Data<MySqlPool>>` extractor added
    // in 0.3.14: `/ws` must still upgrade when no DB pool is registered (the
    // degraded-mode / DB-less test configuration). Group resolution degrades
    // to per-alias identity instead of failing the handshake.
    let app =
        test::init_service(App::new().route("/ws", web::get().to(sockets::ws::ws_index))).await;

    let ticket = sockets::tickets::create_ticket(42);
    let req = test::TestRequest::get()
        .uri(&format!("/ws?ticket={ticket}"))
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
async fn ws_handshake_upgrades_with_valid_ticket() {
    let app =
        test::init_service(App::new().route("/ws", web::get().to(sockets::ws::ws_index))).await;

    let ticket = sockets::tickets::create_ticket(42);
    let req = test::TestRequest::get()
        .uri(&format!("/ws?ticket={ticket}"))
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
