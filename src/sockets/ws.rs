use actix::prelude::*;
use actix_web::{Error, HttpRequest, HttpResponse, web};
use actix_web_actors::ws;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use tracing::instrument;

use crate::sockets::channels::{self, ChannelChange, JoinChannel, LeaveAllChannels, LeaveChannel};
use crate::sockets::presence::{self, JoinPresence, LeavePresence};

/// Monotonic counter paired with a timestamp so message ids stay unique
/// even when several messages are routed within the same second.
static MSG_SEQ: AtomicI64 = AtomicI64::new(0);

/// Monotonic counter giving each connection a unique presence key.
static CONN_SEQ: AtomicUsize = AtomicUsize::new(0);

/// The hub action a processed inbound frame wants the caller to perform.
///
/// The frame routing stays pure: no hub messages are sent from
/// [`WsConn::process_text_frame`]; the caller performs the side effect once it
/// has the connection's own address at hand.
enum FrameOutcome {
    /// No hub action; the frame produced only response envelopes (or nothing).
    None,
    /// The frame completed a successful identify; arm a presence join.
    Identify,
    /// The frame requested a channel join for the given channel.
    JoinChannel { channel_id: i64 },
    /// The frame requested a channel leave for the given channel.
    LeaveChannel { channel_id: i64 },
}

/// Actor handling a single WebSocket connection.
pub struct WsConn {
    /// Authenticated player alias id resolved from the socket ticket on connect.
    pub alias_id: Option<i64>,
    /// Unique id for this connection, used to unregister presence on drop.
    conn_key: usize,
    /// True once the client has successfully identified as the ticketed alias.
    identified: bool,
}

impl WsConn {
    /// Presence message to send when this connection drops, if one was ever
    /// registered (i.e. the client completed a successful identify).
    fn leave_presence(&self) -> Option<LeavePresence> {
        let alias_id = self.alias_id?;
        self.identified.then_some(LeavePresence {
            alias_id,
            conn_key: self.conn_key,
        })
    }

    /// Handle a `v1.players.identify` request against the ticketed alias id.
    ///
    /// Returns the response envelope together with the frame outcome; a
    /// successful identify yields [`FrameOutcome::Identify`], arming the
    /// caller to register presence for this connection.
    fn handle_players_identify(&mut self, data: &Value) -> (Value, FrameOutcome) {
        let Some(alias_id) = self.alias_id else {
            return (
                error_payload(
                    "INVALID_INPUT",
                    "playerAliasId does not match authenticated ticket",
                ),
                FrameOutcome::None,
            );
        };
        match data.get("playerAliasId").and_then(|v| v.as_i64()) {
            None => (
                error_payload("INVALID_INPUT", "playerAliasId is required"),
                FrameOutcome::None,
            ),
            Some(id) if id != alias_id => (
                error_payload(
                    "INVALID_INPUT",
                    "playerAliasId does not match authenticated ticket",
                ),
                FrameOutcome::None,
            ),
            Some(id) => {
                self.identified = true;
                (
                    json!({
                        "res": "v1.players.identify.success",
                        "data": {
                            "aliasId": id,
                            "playerId": format!("player-{}", id),
                        }
                    }),
                    FrameOutcome::Identify,
                )
            }
        }
    }

    /// Handle a `v1.channels.join` request: validate the frame and report the
    /// channel membership the caller should register for this connection.
    fn handle_channels_join(&self, data: &Value) -> (Vec<Value>, FrameOutcome) {
        if !self.identified {
            return (
                vec![error_payload(
                    "INVALID_INPUT",
                    "identify before joining a channel",
                )],
                FrameOutcome::None,
            );
        }
        let Some(channel_id) = data.get("channelId").and_then(|v| v.as_i64()) else {
            return (
                vec![error_payload("INVALID_INPUT", "channelId is required")],
                FrameOutcome::None,
            );
        };
        (Vec::new(), FrameOutcome::JoinChannel { channel_id })
    }

    /// Handle a `v1.channels.leave` request; symmetric to
    /// [`Self::handle_channels_join`].
    fn handle_channels_leave(&self, data: &Value) -> (Vec<Value>, FrameOutcome) {
        if !self.identified {
            return (
                vec![error_payload(
                    "INVALID_INPUT",
                    "identify before leaving a channel",
                )],
                FrameOutcome::None,
            );
        }
        let Some(channel_id) = data.get("channelId").and_then(|v| v.as_i64()) else {
            return (
                vec![error_payload("INVALID_INPUT", "channelId is required")],
                FrameOutcome::None,
            );
        };
        (Vec::new(), FrameOutcome::LeaveChannel { channel_id })
    }

    /// Process a single inbound text frame.
    ///
    /// Returns the response envelopes to send back plus the frame outcome
    /// describing the hub action the caller should perform for this
    /// connection (e.g. arming a presence join on a successful identify).
    fn process_text_frame(&mut self, txt: &str) -> (Vec<Value>, FrameOutcome) {
        let v: Value = match serde_json::from_str(txt) {
            Ok(v) => v,
            Err(e) => {
                return (
                    vec![error_payload("INVALID_JSON", e.to_string())],
                    FrameOutcome::None,
                );
            }
        };
        let req = v.get("req").and_then(|r| r.as_str()).unwrap_or("");
        let data = v.get("data").cloned().unwrap_or(json!({}));
        match req {
            "v1.players.identify" => {
                let (response, outcome) = self.handle_players_identify(&data);
                (vec![response], outcome)
            }
            "v1.channels.message" => (vec![handle_channels_message(&data)], FrameOutcome::None),
            "v1.channels.join" => self.handle_channels_join(&data),
            "v1.channels.leave" => self.handle_channels_leave(&data),
            _ => (
                vec![error_payload(
                    "UNHANDLED_REQUEST",
                    format!("Unknown req: {}", req),
                )],
                FrameOutcome::None,
            ),
        }
    }
}

impl Actor for WsConn {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        self.conn_key = CONN_SEQ.fetch_add(1, Ordering::Relaxed);
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

    fn stopping(&mut self, _ctx: &mut Self::Context) -> Running {
        if let Some(LeavePresence { alias_id, conn_key }) = self.leave_presence() {
            presence::hub().do_send(LeavePresence { alias_id, conn_key });
            channels::hub().do_send(LeaveAllChannels { alias_id, conn_key });
        }
        Running::Stop
    }
}

impl Handler<presence::PresenceChange> for WsConn {
    type Result = ();

    fn handle(&mut self, msg: presence::PresenceChange, ctx: &mut Self::Context) {
        write_payload(ctx, &presence::presence_payload(msg.alias_id, msg.online));
    }
}

impl Handler<ChannelChange> for WsConn {
    type Result = ();

    fn handle(&mut self, msg: ChannelChange, ctx: &mut Self::Context) {
        let payload = if msg.joined {
            channels::player_joined_payload(msg.channel_id, msg.alias_id)
        } else {
            channels::player_left_payload(msg.channel_id, msg.alias_id)
        };
        write_payload(ctx, &payload);
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

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsConn {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Text(txt)) => {
                // Keep-alive ping from the client (bare token, not an envelope).
                if txt == "v1.heartbeat" {
                    ctx.text("v1.heartbeat");
                    return;
                }
                let (payloads, outcome) = self.process_text_frame(&txt);
                match outcome {
                    FrameOutcome::Identify => {
                        // Register presence with the ticketed alias (never the
                        // client-supplied id) so this socket receives broadcast
                        // `v1.players.presence.updated` envelopes and the alias
                        // is announced online.
                        if let Some(alias_id) = self.alias_id {
                            presence::hub().do_send(JoinPresence {
                                alias_id,
                                conn_key: self.conn_key,
                                recipient: ctx.address().recipient(),
                            });
                        }
                    }
                    FrameOutcome::JoinChannel { channel_id } => {
                        // Register channel membership with the ticketed alias
                        // and this connection's own address as recipient.
                        if let Some(alias_id) = self.alias_id {
                            channels::hub().do_send(JoinChannel {
                                channel_id,
                                alias_id,
                                conn_key: self.conn_key,
                                recipient: ctx.address().recipient(),
                            });
                        }
                    }
                    FrameOutcome::LeaveChannel { channel_id } => {
                        if let Some(alias_id) = self.alias_id {
                            channels::hub().do_send(LeaveChannel {
                                channel_id,
                                alias_id,
                                conn_key: self.conn_key,
                            });
                        }
                    }
                    FrameOutcome::None => {}
                }
                for payload in payloads {
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
    ws::start(
        WsConn {
            alias_id,
            conn_key: 0,
            identified: false,
        },
        &r,
        stream,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a conn with the fields set for unit testing.
    fn conn(alias_id: Option<i64>) -> WsConn {
        WsConn {
            alias_id,
            conn_key: 7,
            identified: false,
        }
    }

    /// Process a frame, discarding the just-identified flag.
    fn frames(alias_id: Option<i64>, txt: &str) -> Vec<Value> {
        conn(alias_id).process_text_frame(txt).0
    }

    #[test]
    fn identify_success_maps_alias_id() {
        let mut c = conn(Some(42));
        let (payloads, outcome) =
            c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        assert!(matches!(outcome, FrameOutcome::Identify));
        assert!(c.identified);
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["res"], "v1.players.identify.success");
        assert_eq!(payloads[0]["data"]["aliasId"].as_i64(), Some(42));
        assert_eq!(payloads[0]["data"]["playerId"].as_str(), Some("player-42"));
    }

    #[test]
    fn identify_error_paths_do_not_identify() {
        let mut c = conn(Some(1));
        let (payloads, outcome) =
            c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":2}}"#);
        assert!(matches!(outcome, FrameOutcome::None));
        assert!(!c.identified);
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));

        let mut c = conn(Some(1));
        let (_, outcome) = c.process_text_frame(r#"{"req":"v1.players.identify","data":{}}"#);
        assert!(matches!(outcome, FrameOutcome::None));
        assert!(!c.identified);

        let mut c = conn(None);
        let (_, outcome) =
            c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":1}}"#);
        assert!(matches!(outcome, FrameOutcome::None));
        assert!(!c.identified);
    }

    #[test]
    fn identify_missing_alias_id_errors() {
        let payloads = frames(Some(42), r#"{"req":"v1.players.identify","data":{}}"#);
        assert_eq!(payloads[0]["res"], "v1.error");
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn identify_claiming_another_alias_errors() {
        let payloads = frames(
            Some(1),
            r#"{"req":"v1.players.identify","data":{"playerAliasId":2}}"#,
        );
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn identify_without_authenticated_ticket_errors() {
        let payloads = frames(
            None,
            r#"{"req":"v1.players.identify","data":{"playerAliasId":1}}"#,
        );
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn join_uses_resolved_ticket_alias_not_client_claim() {
        // The client may only claim its ticketed alias; the presence join is
        // keyed on the server-resolved alias id, never a body-supplied value.
        let mut c = conn(Some(7));
        let (payloads, outcome) =
            c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":7}}"#);
        assert!(matches!(outcome, FrameOutcome::Identify));
        assert_eq!(payloads[0]["res"], "v1.players.identify.success");

        let leave = c.leave_presence().expect("identified conn must register");
        assert_eq!(leave.alias_id, 7);
        assert_eq!(leave.conn_key, c.conn_key);
    }

    #[test]
    fn leave_presence_only_when_identified() {
        assert!(conn(Some(42)).leave_presence().is_none());

        let mut c = conn(Some(42));
        let (_, outcome) =
            c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        assert!(matches!(outcome, FrameOutcome::Identify));
        let leave = c.leave_presence().expect("identified conn must register");
        assert_eq!(leave.alias_id, 42);
        assert_eq!(leave.conn_key, c.conn_key);
    }

    #[test]
    fn channels_message_success() {
        let payloads = frames(
            Some(3),
            r#"{"req":"v1.channels.message","data":{"channelId":7,"message":"hi"}}"#,
        );
        assert_eq!(payloads[0]["res"], "v1.channels.message");
        assert_eq!(payloads[0]["data"]["channel"]["id"].as_i64(), Some(7));
        assert_eq!(
            payloads[0]["data"]["message"]["message"].as_str(),
            Some("hi")
        );
    }

    #[test]
    fn channels_message_missing_channel_id_errors() {
        let payloads = frames(
            Some(1),
            r#"{"req":"v1.channels.message","data":{"message":"hi"}}"#,
        );
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn channels_message_missing_or_empty_message_errors() {
        let payloads = frames(
            Some(1),
            r#"{"req":"v1.channels.message","data":{"channelId":1}}"#,
        );
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
        let payloads = frames(
            Some(1),
            r#"{"req":"v1.channels.message","data":{"channelId":1,"message":""}}"#,
        );
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn message_ids_unique_within_same_second() {
        let a = frames(
            Some(1),
            r#"{"req":"v1.channels.message","data":{"channelId":1,"message":"hi"}}"#,
        );
        let b = frames(
            Some(1),
            r#"{"req":"v1.channels.message","data":{"channelId":1,"message":"hi"}}"#,
        );
        assert_ne!(a[0]["data"]["message"]["id"], b[0]["data"]["message"]["id"]);
    }

    #[test]
    fn join_success_yields_join_action() {
        let mut c = conn(Some(42));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        let (payloads, outcome) =
            c.process_text_frame(r#"{"req":"v1.channels.join","data":{"channelId":7}}"#);
        assert!(
            payloads.is_empty(),
            "join success must send no response yet"
        );
        assert!(matches!(
            outcome,
            FrameOutcome::JoinChannel { channel_id: 7 }
        ));
    }

    #[test]
    fn join_missing_channel_id_errors() {
        let mut c = conn(Some(42));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        let (payloads, outcome) = c.process_text_frame(r#"{"req":"v1.channels.join","data":{}}"#);
        assert!(matches!(outcome, FrameOutcome::None));
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn join_before_identify_errors() {
        let (payloads, outcome) = conn(Some(42))
            .process_text_frame(r#"{"req":"v1.channels.join","data":{"channelId":7}}"#);
        assert!(matches!(outcome, FrameOutcome::None));
        assert_eq!(
            payloads[0]["data"]["code"].as_str(),
            Some("INVALID_INPUT"),
            "a channel must not be joinable before identify"
        );
    }

    #[test]
    fn leave_success_yields_leave_action() {
        let mut c = conn(Some(42));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        let (payloads, outcome) =
            c.process_text_frame(r#"{"req":"v1.channels.leave","data":{"channelId":7}}"#);
        assert!(
            payloads.is_empty(),
            "leave success must send no response yet"
        );
        assert!(matches!(
            outcome,
            FrameOutcome::LeaveChannel { channel_id: 7 }
        ));
    }

    #[test]
    fn leave_missing_channel_id_errors() {
        let mut c = conn(Some(42));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        let (payloads, outcome) = c.process_text_frame(r#"{"req":"v1.channels.leave","data":{}}"#);
        assert!(matches!(outcome, FrameOutcome::None));
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn leave_before_identify_errors() {
        let (payloads, outcome) = conn(Some(42))
            .process_text_frame(r#"{"req":"v1.channels.leave","data":{"channelId":7}}"#);
        assert!(matches!(outcome, FrameOutcome::None));
        assert_eq!(
            payloads[0]["data"]["code"].as_str(),
            Some("INVALID_INPUT"),
            "a channel must not be leavable before identify"
        );
    }

    #[test]
    fn unknown_request_errors() {
        let payloads = frames(Some(1), r#"{"req":"v1.nope","data":{}}"#);
        assert_eq!(
            payloads[0]["data"]["code"].as_str(),
            Some("UNHANDLED_REQUEST")
        );
    }

    #[test]
    fn invalid_json_errors() {
        let payloads = frames(Some(1), "not-json");
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_JSON"));
    }
}
