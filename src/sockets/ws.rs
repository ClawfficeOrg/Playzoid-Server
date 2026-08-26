use actix::prelude::*;
use actix_web::{Error, HttpRequest, HttpResponse, web};
use actix_web_actors::ws;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicI64, Ordering};
use tracing::instrument;

/// Monotonic counter paired with a timestamp so message ids stay unique
/// even when several messages are routed within the same second.
static MSG_SEQ: AtomicI64 = AtomicI64::new(0);

/// Actor handling a single WebSocket connection.
pub struct WsConn {
    /// Authenticated player alias id resolved from the socket ticket on connect.
    pub alias_id: Option<i64>,
}

impl Actor for WsConn {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        if self.alias_id.is_none() {
            // Reject unauthenticated connections with the Talo error envelope.
            write_payload(
                ctx,
                &error_payload("INVALID_SOCKET_TOKEN", "Missing or invalid socket ticket"),
            );
            ctx.close(Some(ws::CloseCode::Policy.into()));
            ctx.stop();
        }
    }
}

/// Serialize an inbound error envelope (`v1.error`).
fn error_payload(code: &str, message: impl ToString) -> Value {
    json!({
        "res": "v1.error",
        "data": { "code": code, "message": message.to_string() }
    })
}

/// Serialize and send a single response envelope over the socket.
fn write_payload(ctx: &mut ws::WebsocketContext<WsConn>, payload: &Value) {
    if let Ok(s) = serde_json::to_string(payload) {
        ctx.text(s);
    }
}

/// Handle a `v1.players.identify` request against the ticketed alias id.
fn handle_players_identify(alias_id: Option<i64>, data: &Value) -> Value {
    let Some(alias_id) = alias_id else {
        return error_payload(
            "INVALID_INPUT",
            "playerAliasId does not match authenticated ticket",
        );
    };
    match data.get("playerAliasId").and_then(|v| v.as_i64()) {
        None => error_payload("INVALID_INPUT", "playerAliasId is required"),
        Some(id) if id != alias_id => error_payload(
            "INVALID_INPUT",
            "playerAliasId does not match authenticated ticket",
        ),
        Some(id) => json!({
            "res": "v1.players.identify.success",
            "data": {
                "aliasId": id,
                "playerId": format!("player-{}", id),
            }
        }),
    }
}

/// Handle a `v1.channels.message` request; echoes the message into the channel.
fn handle_channels_message(data: &Value) -> Value {
    let channel_id = match data.get("channelId").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => return error_payload("INVALID_INPUT", "channelId is required"),
    };
    let message = match data.get("message").and_then(|v| v.as_str()) {
        Some(m) if !m.is_empty() => m,
        _ => return error_payload("INVALID_INPUT", "message is required"),
    };
    let id = format!(
        "msg-{}-{}",
        chrono::Utc::now().timestamp(),
        MSG_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    json!({
        "res": "v1.channels.message",
        "data": {
            "channel": { "id": channel_id },
            "message": {
                "id": id,
                "from": "server",
                "message": message,
            }
        }
    })
}

/// Process a single inbound text frame and return the response envelopes to send back.
fn process_text_frame(alias_id: Option<i64>, txt: &str) -> Vec<Value> {
    let v: Value = match serde_json::from_str(txt) {
        Ok(v) => v,
        Err(e) => return vec![error_payload("INVALID_JSON", e.to_string())],
    };
    let req = v.get("req").and_then(|r| r.as_str()).unwrap_or("");
    let data = v.get("data").cloned().unwrap_or(json!({}));
    let response = match req {
        "v1.players.identify" => handle_players_identify(alias_id, &data),
        "v1.channels.message" => handle_channels_message(&data),
        _ => error_payload("UNHANDLED_REQUEST", format!("Unknown req: {}", req)),
    };
    vec![response]
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsConn {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Text(txt)) => {
                // Keep-alive ping from the client (bare token, not an envelope).
                if txt == "v1.heartbeat" {
                    ctx.text("v1.heartbeat");
                    return;
                }
                for payload in process_text_frame(self.alias_id, &txt) {
                    write_payload(ctx, &payload);
                }
            }
            Ok(ws::Message::Ping(b)) => ctx.pong(&b),
            Ok(ws::Message::Pong(_)) => {}
            Ok(ws::Message::Binary(_)) => {}
            Ok(ws::Message::Close(reason)) => {
                ctx.close(reason);
                ctx.stop();
            }
            _ => {}
        }
    }
}

/// HTTP handler upgrading a request to a WebSocket session.
///
/// Authentication uses the one-shot ticket issued by `POST /v1/socket-tickets`
/// and passed as the `?ticket=` query parameter, matching the Talo flow. A
/// missing or invalid ticket still upgrades the connection so the client can
/// receive the `INVALID_SOCKET_TOKEN` error envelope, then the connection is
/// closed by [`WsConn::started`].
#[instrument(skip_all)]
pub async fn ws_index(r: HttpRequest, stream: web::Payload) -> Result<HttpResponse, Error> {
    // Extract ticket from query param: ?ticket=...
    let ticket_opt = url::form_urlencoded::parse(r.query_string().as_bytes())
        .find(|(k, _)| k == "ticket")
        .map(|(_, v)| v.into_owned());
    let alias_id = ticket_opt
        .as_deref()
        .and_then(crate::sockets::tickets::verify_ticket);
    ws::start(WsConn { alias_id }, &r, stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_success_maps_alias_id() {
        let frames = process_text_frame(
            Some(42),
            r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#,
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["res"], "v1.players.identify.success");
        assert_eq!(frames[0]["data"]["aliasId"].as_i64(), Some(42));
        assert_eq!(frames[0]["data"]["playerId"].as_str(), Some("player-42"));
    }

    #[test]
    fn identify_missing_alias_id_errors() {
        let frames = process_text_frame(Some(42), r#"{"req":"v1.players.identify","data":{}}"#);
        assert_eq!(frames[0]["res"], "v1.error");
        assert_eq!(frames[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn identify_claiming_another_alias_errors() {
        let frames = process_text_frame(
            Some(1),
            r#"{"req":"v1.players.identify","data":{"playerAliasId":2}}"#,
        );
        assert_eq!(frames[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn identify_without_authenticated_ticket_errors() {
        let frames = process_text_frame(
            None,
            r#"{"req":"v1.players.identify","data":{"playerAliasId":1}}"#,
        );
        assert_eq!(frames[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn channels_message_success() {
        let frames = process_text_frame(
            Some(3),
            r#"{"req":"v1.channels.message","data":{"channelId":7,"message":"hi"}}"#,
        );
        assert_eq!(frames[0]["res"], "v1.channels.message");
        assert_eq!(frames[0]["data"]["channel"]["id"].as_i64(), Some(7));
        assert_eq!(frames[0]["data"]["message"]["message"].as_str(), Some("hi"));
    }

    #[test]
    fn channels_message_missing_channel_id_errors() {
        let frames = process_text_frame(
            Some(1),
            r#"{"req":"v1.channels.message","data":{"message":"hi"}}"#,
        );
        assert_eq!(frames[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn channels_message_missing_or_empty_message_errors() {
        let frames = process_text_frame(
            Some(1),
            r#"{"req":"v1.channels.message","data":{"channelId":1}}"#,
        );
        assert_eq!(frames[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
        let frames = process_text_frame(
            Some(1),
            r#"{"req":"v1.channels.message","data":{"channelId":1,"message":""}}"#,
        );
        assert_eq!(frames[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn message_ids_unique_within_same_second() {
        let a = process_text_frame(
            Some(1),
            r#"{"req":"v1.channels.message","data":{"channelId":1,"message":"hi"}}"#,
        );
        let b = process_text_frame(
            Some(1),
            r#"{"req":"v1.channels.message","data":{"channelId":1,"message":"hi"}}"#,
        );
        assert_ne!(a[0]["data"]["message"]["id"], b[0]["data"]["message"]["id"]);
    }

    #[test]
    fn unknown_request_errors() {
        let frames = process_text_frame(Some(1), r#"{"req":"v1.nope","data":{}}"#);
        assert_eq!(
            frames[0]["data"]["code"].as_str(),
            Some("UNHANDLED_REQUEST")
        );
    }

    #[test]
    fn invalid_json_errors() {
        let frames = process_text_frame(Some(1), "not-json");
        assert_eq!(frames[0]["data"]["code"].as_str(), Some("INVALID_JSON"));
    }
}
