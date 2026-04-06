use actix::prelude::*;
use actix_web::{web, Error, HttpRequest, HttpResponse};
use actix_web_actors::ws;
use serde_json::{json, Value};

/// Actor handling a single WebSocket connection
pub struct WsConn;

impl Actor for WsConn {
    type Context = ws::WebsocketContext<Self>;
}

fn send_error<E: ToString>(ctx: &mut ws::WebsocketContext<WsConn>, code: &str, message: E) {
    let payload = json!({ "res": "v1.error", "data": { "code": code, "message": message.to_string() } });
    if let Ok(s) = serde_json::to_string(&payload) {
        ctx.text(s);
    }
}

fn handle_players_identify(ctx: &mut ws::WebsocketContext<WsConn>, data: Value) {
    // Minimal stub: accept playerAliasId and return identify.success with alias and session placeholders
    let alias_id = data.get("playerAliasId").and_then(|v| v.as_i64()).unwrap_or(0);
    let resp = json!({
        "res": "v1.players.identify.success",
        "data": {
            "aliasId": alias_id,
            "playerId": format!("player-{}", alias_id),
        }
    });
    if let Ok(s) = serde_json::to_string(&resp) {
        ctx.text(s);
    }
}

fn handle_channels_message(ctx: &mut ws::WebsocketContext<WsConn>, data: Value) {
    // Expect channelId and message
    let channel_id = data.get("channelId").and_then(|v| v.as_i64()).unwrap_or(0);
    let message = data.get("message").and_then(|v| v.as_str()).unwrap_or("");

    let resp = json!({
        "res": "v1.channels.message",
        "data": {
            "channel": { "id": channel_id },
            "message": {
                "id": format!("msg-{}", chrono::Utc::now().timestamp()),
                "from": "server",
                "message": message
            }
        }
    });
    if let Ok(s) = serde_json::to_string(&resp) {
        ctx.text(s);
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsConn {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Text(txt)) => {
                // Parse expected envelope: { req: string, data: object }
                if txt == "v1.heartbeat" {
                    // keep alive ping from client
                    ctx.text("v1.heartbeat");
                    return;
                }

                let parsed: Result<Value, _> = serde_json::from_str(&txt);
                match parsed {
                    Ok(v) => {
                        let req = v.get("req").and_then(|r| r.as_str()).unwrap_or("");
                        let data = v.get("data").cloned().unwrap_or(json!({}));
                        match req {
                            "v1.players.identify" => handle_players_identify(ctx, data),
                            "v1.channels.message" => handle_channels_message(ctx, data),
                            _ => send_error(ctx, "UNHANDLED_REQUEST", format!("Unknown req: {}", req)),
                        }
                    }
                    Err(e) => {
                        send_error(ctx, "INVALID_JSON", e.to_string());
                    }
                }
            }
            Ok(ws::Message::Ping(b)) => ctx.pong(&b),
            Ok(ws::Message::Pong(_)) => {},
            Ok(ws::Message::Binary(_)) => {},
            Ok(ws::Message::Close(reason)) => {
                ctx.close(reason);
                ctx.stop();
            }
            _ => {}
        }
    }
}

pub async fn ws_index(r: HttpRequest, stream: web::Payload) -> Result<HttpResponse, Error> {
    ws::start(WsConn {}, &r, stream)
}
