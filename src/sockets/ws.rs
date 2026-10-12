use actix::prelude::*;
use actix_web::{Error, HttpRequest, HttpResponse, web};
use actix_web_actors::ws;
use serde_json::{Value, json};
use sqlx::MySqlPool;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::instrument;

use crate::sockets::channels::{
    self, ChannelMessage, ChannelNotification, JoinChannel, LeaveAllChannels, LeaveChannel,
    MAX_CHAT_MESSAGE_CHARS,
};
use crate::sockets::groups;
use crate::sockets::presence::{self, JoinPresence, LeavePresence};

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
    /// The frame requested a channel join for the given channel; the resolved
    /// `parent_account_id` derives the participant group the hub registers.
    JoinChannel {
        channel_id: i64,
        parent_account_id: Option<i64>,
    },
    /// The frame requested a channel leave for the given channel.
    LeaveChannel { channel_id: i64 },
    /// The frame requested a chat message for the given channel; broadcast it.
    BroadcastMessage(ChannelMessage),
}

/// Actor handling a single WebSocket connection.
pub struct WsConn {
    /// Authenticated player alias id resolved from the socket ticket on connect.
    pub alias_id: Option<i64>,
    /// Server-resolved `players.parent_account_id` for the ticketed alias
    /// (`None` for a root account, an unresolvable alias, or when the DB pool
    /// is unavailable). Derives the channel-participation group key.
    pub parent_account_id: Option<i64>,
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
                            "parentAccountId": self.parent_account_id,
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
        (
            Vec::new(),
            FrameOutcome::JoinChannel {
                channel_id,
                parent_account_id: self.parent_account_id,
            },
        )
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

    /// Handle a `v1.channels.message` request: validate the frame and arm a
    /// chat broadcast for the channel. The sender alias is always the
    /// server-resolved socket-ticket alias — a client-supplied
    /// `playerAliasId` claim is ignored. The actual fan-out happens in the
    /// hub, gated on the sender being a member of the channel.
    fn handle_channels_message(&self, data: &Value) -> (Vec<Value>, FrameOutcome) {
        if !self.identified {
            return (
                vec![error_payload(
                    "INVALID_INPUT",
                    "identify before sending a channel message",
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
        let Some(message) = data.get("message").and_then(|v| v.as_str()) else {
            return (
                vec![error_payload("INVALID_INPUT", "message is required")],
                FrameOutcome::None,
            );
        };
        if message.is_empty() {
            return (
                vec![error_payload("INVALID_INPUT", "message is required")],
                FrameOutcome::None,
            );
        }
        if message.chars().count() > MAX_CHAT_MESSAGE_CHARS {
            return (
                vec![error_payload("INVALID_INPUT", "message is too long")],
                FrameOutcome::None,
            );
        }
        let Some(alias_id) = self.alias_id else {
            return (
                vec![error_payload(
                    "INVALID_INPUT",
                    "playerAliasId does not match authenticated ticket",
                )],
                FrameOutcome::None,
            );
        };
        (
            Vec::new(),
            FrameOutcome::BroadcastMessage(ChannelMessage {
                channel_id,
                alias_id,
                // The sender's participant group is stamped here, server-side,
                // from the ticketed alias and its resolved parent — never from
                // a client-supplied value. The hub gates the fan-out on it.
                group: groups::group_key(alias_id, self.parent_account_id),
                message: message.to_string(),
            }),
        )
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
            "v1.channels.message" => self.handle_channels_message(&data),
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
        crate::middleware::metrics::metrics().ws_connected();
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
        crate::middleware::metrics::metrics().ws_disconnected();
        if let Some(LeavePresence { alias_id, conn_key }) = self.leave_presence() {
            presence::hub().do_send(LeavePresence { alias_id, conn_key });
            channels::hub().do_send(LeaveAllChannels { conn_key });
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

impl Handler<ChannelNotification> for WsConn {
    type Result = ();

    fn handle(&mut self, msg: ChannelNotification, ctx: &mut Self::Context) {
        let payload = match msg {
            ChannelNotification::Change(change) => {
                if change.joined {
                    channels::player_joined_payload(change.channel_id, change.alias_id)
                } else {
                    channels::player_left_payload(change.channel_id, change.alias_id)
                }
            }
            ChannelNotification::Message(message) => channels::channel_message_payload(
                message.channel_id,
                message.alias_id,
                &message.message,
            ),
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
                    FrameOutcome::JoinChannel {
                        channel_id,
                        parent_account_id,
                    } => {
                        // Register channel membership with the ticketed alias
                        // and this connection's own address as recipient, so it
                        // receives both membership-change and chat-message
                        // notifications for the channel. The hub derives the
                        // participant group from the resolved parent id.
                        if let Some(alias_id) = self.alias_id {
                            channels::hub().do_send(JoinChannel {
                                channel_id,
                                alias_id,
                                parent_account_id,
                                conn_key: self.conn_key,
                                recipient: ctx.address().recipient::<ChannelNotification>(),
                            });
                        }
                    }
                    FrameOutcome::LeaveChannel { channel_id } => {
                        if let Some(alias_id) = self.alias_id {
                            channels::hub().do_send(LeaveChannel {
                                channel_id,
                                alias_id,
                                parent_account_id: self.parent_account_id,
                                conn_key: self.conn_key,
                            });
                        }
                    }
                    FrameOutcome::BroadcastMessage(message) => {
                        // The hub fans the message out to every member of the
                        // channel (sender included); the sender alias is the
                        // ticketed one stamped by process_text_frame.
                        channels::hub().do_send(message);
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
pub async fn ws_index(
    r: HttpRequest,
    stream: web::Payload,
    pool: Option<web::Data<MySqlPool>>,
) -> Result<HttpResponse, Error> {
    // Extract ticket from query param: ?ticket=...
    let ticket_opt = url::form_urlencoded::parse(r.query_string().as_bytes())
        .find(|(k, _)| k == "ticket")
        .map(|(_, v)| v.into_owned());
    let alias_id = ticket_opt
        .as_deref()
        .and_then(crate::sockets::tickets::verify_ticket);
    // Resolve the subaccount parent for the ticketed alias so channel
    // participation can be grouped. Best-effort: a missing pool, an unknown
    // alias, or a failed lookup degrades to `None` (the alias groups as
    // itself) without ever failing the connection.
    let parent_account_id = match pool.as_ref().zip(alias_id) {
        Some((pool, alias_id)) => {
            match groups::resolve_parent_account_id(pool.get_ref(), alias_id).await {
                Ok(row) => row.flatten(),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        alias_id,
                        "subaccount group lookup failed; treating alias as its own group"
                    );
                    None
                }
            }
        }
        None => None,
    };
    ws::start(
        WsConn {
            alias_id,
            parent_account_id,
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
            parent_account_id: None,
            conn_key: 7,
            identified: false,
        }
    }

    /// Build a conn whose resolved parent is set (subaccount simulation).
    fn conn_with_parent(alias_id: i64, parent_account_id: Option<i64>) -> WsConn {
        WsConn {
            alias_id: Some(alias_id),
            parent_account_id,
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
        assert!(
            payloads[0]["data"]["parentAccountId"].is_null(),
            "root conns surface a null parentAccountId"
        );
    }

    #[test]
    fn identify_success_surfaces_resolved_parent() {
        // A subaccount conn carries its server-resolved parent into the
        // additive `parentAccountId` field of the identify.success data.
        let mut c = conn_with_parent(42, Some(7));
        let (payloads, outcome) =
            c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        assert!(matches!(outcome, FrameOutcome::Identify));
        assert_eq!(payloads[0]["data"]["parentAccountId"].as_i64(), Some(7));
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
    fn channels_message_broadcast_outcome() {
        // An identified member's message arms a hub broadcast; the sender
        // alias comes from the socket ticket, never a body claim.
        let mut c = conn(Some(42));
        let (_, outcome) =
            c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        assert!(matches!(outcome, FrameOutcome::Identify));
        let (payloads, outcome) = c.process_text_frame(
            r#"{"req":"v1.channels.message","data":{"channelId":7,"message":"hi","playerAliasId":999}}"#,
        );
        assert!(
            payloads.is_empty(),
            "a message send must produce no local echo; the hub broadcasts it"
        );
        match outcome {
            FrameOutcome::BroadcastMessage(message) => {
                assert_eq!(message.channel_id, 7);
                assert_eq!(message.message, "hi");
                assert_eq!(
                    message.alias_id, 42,
                    "sender alias must be the ticketed alias, never the spoofed body claim"
                );
                assert_eq!(
                    message.group, 42,
                    "a root conn's message must be stamped with the alias as its own group"
                );
            }
            _ => panic!("expected BroadcastMessage outcome"),
        }
    }

    #[test]
    fn channels_message_stamps_group_for_subaccount() {
        // A subaccount conn (alias 42, parent 5) stamps its message with the
        // parent-derived group (5); a root conn stamps the alias as the group.
        let mut c = conn_with_parent(42, Some(5));
        let (_, outcome) =
            c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        assert!(matches!(outcome, FrameOutcome::Identify));
        let (payloads, outcome) = c.process_text_frame(
            r#"{"req":"v1.channels.message","data":{"channelId":7,"message":"hi","playerAliasId":999}}"#,
        );
        assert!(
            payloads.is_empty(),
            "a message send must produce no local echo; the hub broadcasts it"
        );
        match outcome {
            FrameOutcome::BroadcastMessage(message) => {
                assert_eq!(
                    message.group, 5,
                    "group must derive from the resolved parent"
                );
                assert_eq!(message.alias_id, 42);
            }
            _ => panic!("expected BroadcastMessage outcome"),
        }

        // Root conn: same alias shape but no parent -> group is the alias.
        let mut c = conn(Some(42));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        let (_, outcome) = c.process_text_frame(
            r#"{"req":"v1.channels.message","data":{"channelId":7,"message":"hi"}}"#,
        );
        match outcome {
            FrameOutcome::BroadcastMessage(message) => {
                assert_eq!(message.group, 42, "root conns group as their own alias");
            }
            _ => panic!("expected BroadcastMessage outcome"),
        }
    }

    #[test]
    fn channels_message_before_identify_errors() {
        let (payloads, outcome) = conn(Some(42)).process_text_frame(
            r#"{"req":"v1.channels.message","data":{"channelId":7,"message":"hi"}}"#,
        );
        assert!(
            matches!(outcome, FrameOutcome::None),
            "a message must not be broadcast before identify"
        );
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn channels_message_missing_channel_id_errors() {
        let mut c = conn(Some(1));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":1}}"#);
        let (payloads, outcome) =
            c.process_text_frame(r#"{"req":"v1.channels.message","data":{"message":"hi"}}"#);
        assert!(matches!(outcome, FrameOutcome::None));
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn channels_message_missing_or_empty_message_errors() {
        let mut c = conn(Some(1));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":1}}"#);
        let (payloads, outcome) =
            c.process_text_frame(r#"{"req":"v1.channels.message","data":{"channelId":1}}"#);
        assert!(matches!(outcome, FrameOutcome::None));
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));

        let mut c = conn(Some(1));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":1}}"#);
        let (payloads, outcome) = c.process_text_frame(
            r#"{"req":"v1.channels.message","data":{"channelId":1,"message":""}}"#,
        );
        assert!(matches!(outcome, FrameOutcome::None));
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
    }

    #[test]
    fn channels_message_at_max_length_ok_oversize_errors() {
        let max_ok = "x".repeat(MAX_CHAT_MESSAGE_CHARS);
        let frame = format!(
            r#"{{"req":"v1.channels.message","data":{{"channelId":1,"message":"{max_ok}"}}}}"#
        );
        let mut c = conn(Some(1));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":1}}"#);
        let (payloads, outcome) = c.process_text_frame(&frame);
        assert!(
            payloads.is_empty(),
            "a message at the character cap must be accepted"
        );
        assert!(matches!(outcome, FrameOutcome::BroadcastMessage(_)));

        let too_long = "x".repeat(MAX_CHAT_MESSAGE_CHARS + 1);
        let frame = format!(
            r#"{{"req":"v1.channels.message","data":{{"channelId":1,"message":"{too_long}"}}}}"#
        );
        let mut c = conn(Some(1));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":1}}"#);
        let (payloads, outcome) = c.process_text_frame(&frame);
        assert!(matches!(outcome, FrameOutcome::None));
        assert_eq!(payloads[0]["data"]["code"].as_str(), Some("INVALID_INPUT"));
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
            FrameOutcome::JoinChannel {
                channel_id: 7,
                parent_account_id: None
            }
        ));
    }

    #[test]
    fn join_surfaces_resolved_parent_for_grouping() {
        // A subaccount conn's join outcome carries its resolved parent so the
        // hub can derive the participant group (root conns pass None).
        let mut c = conn_with_parent(42, Some(5));
        c.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        let (_, outcome) =
            c.process_text_frame(r#"{"req":"v1.channels.join","data":{"channelId":7}}"#);
        assert!(matches!(
            outcome,
            FrameOutcome::JoinChannel {
                channel_id: 7,
                parent_account_id: Some(5)
            }
        ));
        let mut root = conn(Some(42));
        root.process_text_frame(r#"{"req":"v1.players.identify","data":{"playerAliasId":42}}"#);
        let (_, root_outcome) =
            root.process_text_frame(r#"{"req":"v1.channels.join","data":{"channelId":7}}"#);
        assert!(matches!(
            root_outcome,
            FrameOutcome::JoinChannel {
                channel_id: 7,
                parent_account_id: None
            }
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
