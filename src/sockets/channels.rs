//! In-memory WebSocket game-channel membership registry.
//!
//! Tracks which **participant groups** are members of which channels and
//! broadcasts the Talo `v1.channels.player-joined` / `v1.channels.player-left`
//! envelopes to member connections when participation changes, plus the
//! `v1.channels.message` envelope when a participant sends a chat message.
//!
//! Participation is grouped by subaccount parent: `group(alias)` is the
//! server-resolved `players.parent_account_id` (see [`crate::sockets::groups`]),
//! or the alias itself for root accounts. A channel's participant set is its
//! distinct groups with at least one live connection; the first connection of a
//! group announces `player-joined` (carrying the joining alias), the last
//! connection announces `player-left` (carrying the departing alias), and chat
//! fans out to every connection across the channel's participant groups. A
//! subaccount and its parent therefore share channel membership and each
//! other's chat — the "grouping" — while presence (0.3.11) stays per-alias so
//! subaccounts still appear as distinct users.
//!
//! Upstream Talo drives channel membership over HTTP and only fans out the
//! membership changes and messages over sockets; `v1.channels.join` /
//! `v1.channels.leave` / `v1.channels.message` are Playzoid request extensions
//! (the only in-scope trigger while no channel persistence exists). The
//! response envelopes stay Talo-verified and carry per-alias `playerAlias`
//! ids — never the parent relationship.

use actix::prelude::*;
use once_cell::sync::Lazy;
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::sockets::groups;

/// Upper bound of live connections tracked per (channel, group) membership.
/// Guards against unbounded memory growth if a recipient's `stopping()`-issued
/// leave is ever missed; exceeding it triggers a prune of dead recipients
/// instead of failing the join.
const MAX_CONNS_PER_MEMBER: usize = 256;

/// Upper bound of an inbound chat message length in characters. Rejects
/// oversize frames at the websocket layer before they can reach the hub.
pub const MAX_CHAT_MESSAGE_CHARS: usize = 1000;

/// Numeric `GameChannelLeavingReason::DEFAULT` (serialized as an integer, per
/// the upstream TS numeric enum).
const PLAYER_LEFT_REASON_DEFAULT: i64 = 0;

/// A channel membership transition: `joined: true` when the first connection
/// for a participant group registers in a channel, `joined: false` when the
/// last connection of that group drops. The carried alias is the joining or
/// departing alias, never the group key.
#[derive(Message, Clone)]
#[rtype(result = "()")]
pub struct ChannelChange {
    /// Channel the transition belongs to.
    pub channel_id: i64,
    /// Player alias the transition belongs to (the alias that joined/left).
    pub alias_id: i64,
    /// `true` when the player joined the channel, `false` when it left.
    pub joined: bool,
}

/// A chat message sent by one channel participant, to be broadcast to every
/// connection across the channel's participant groups (sender included).
#[derive(Message, Clone)]
#[rtype(result = "()")]
pub struct ChannelMessage {
    /// Channel the message belongs to.
    pub channel_id: i64,
    /// Player alias the message was sent from (always the server-resolved
    /// socket-ticket alias, never a client-supplied id).
    pub alias_id: i64,
    /// The sender's participant group key, stamped at the websocket layer from
    /// the ticketed alias and its server-resolved parent. Membership and the
    /// broadcast gate are evaluated at group level.
    pub group: i64,
    /// The chat text, validated and length-capped at the websocket layer.
    pub message: String,
}

/// A fan-out envelope a channel member connection can receive. A single
/// recipient per connection is registered in the hub and reused for both the
/// membership-change and chat-message fan-outs, so no connection needs to be
/// tracked twice for the two envelope kinds.
#[derive(Message, Clone)]
#[rtype(result = "()")]
pub enum ChannelNotification {
    /// A membership transition (player joined / left).
    Change(ChannelChange),
    /// A chat message broadcast within the channel.
    Message(ChannelMessage),
}

/// Register a connection under a player alias in a channel.
#[derive(Message)]
#[rtype(result = "()")]
pub struct JoinChannel {
    /// Channel the connection is joining.
    pub channel_id: i64,
    /// Player alias the connection belongs to (always the server-resolved
    /// socket-ticket alias, never a client-supplied id).
    pub alias_id: i64,
    /// The alias's server-resolved `parent_account_id` (`None` for a root
    /// account), from which the hub derives the participant group key.
    pub parent_account_id: Option<i64>,
    /// Unique connection key so the same connection can be unregistered later.
    pub conn_key: usize,
    /// Recipient to push subsequent channel notifications (changes + messages).
    pub recipient: Recipient<ChannelNotification>,
}

/// Remove a connection from a single channel.
#[derive(Message)]
#[rtype(result = "()")]
pub struct LeaveChannel {
    /// Channel the connection is leaving.
    pub channel_id: i64,
    /// Player alias the connection was registered under.
    pub alias_id: i64,
    /// The alias's server-resolved `parent_account_id` (derives the participant
    /// group, matching the join).
    pub parent_account_id: Option<i64>,
    /// Unique connection key of the departing connection.
    pub conn_key: usize,
}

/// Remove a connection from every channel it joined.
#[derive(Message)]
#[rtype(result = "()")]
pub struct LeaveAllChannels {
    /// Unique connection key of the departing connection.
    pub conn_key: usize,
}

/// Connection registry keyed by channel id, then participant group, then alias
/// id, then conn key.
type ChannelMemberships =
    HashMap<i64, HashMap<i64, HashMap<i64, HashMap<usize, Recipient<ChannelNotification>>>>>;

/// Reverse index mapping a connection key to the memberships it holds, so a
/// dropped connection can be unregistered in O(its channels) not O(all).
/// Entries are (channel, group, alias) tuples — the alias is needed so the
/// group's last-conn `player-left` announces the departing alias.
type ConnMemberships = HashMap<usize, Vec<(i64, i64, i64)>>;

/// Actor owning the channel membership registry.
#[derive(Default)]
pub struct ChannelHub {
    /// Live memberships keyed by channel, then group, then alias, then conn key.
    channels: ChannelMemberships,
    /// Reverse index: connection key -> (channel, group, alias) it joined.
    conn_channels: ConnMemberships,
}

impl ChannelHub {
    /// Create an empty channel hub.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fan a channel change out to every connection currently in the channel.
    fn broadcast_channel_change(&self, channel_id: i64, change: &ChannelChange) {
        if let Some(groups) = self.channels.get(&channel_id) {
            for aliases in groups.values() {
                for conns in aliases.values() {
                    for recipient in conns.values() {
                        recipient.do_send(ChannelNotification::Change(change.clone()));
                    }
                }
            }
        }
    }

    /// Fan a chat message out to every connection currently in the channel,
    /// the sender included (mirrors the membership fan-out).
    fn broadcast_channel_message(&self, msg: &ChannelMessage) {
        if let Some(groups) = self.channels.get(&msg.channel_id) {
            for aliases in groups.values() {
                for conns in aliases.values() {
                    for recipient in conns.values() {
                        recipient.do_send(ChannelNotification::Message(msg.clone()));
                    }
                }
            }
        }
    }

    /// Drop recipients whose underlying actor has stopped, removing empty
    /// alias entries and dropping the participant group (and channel) entirely
    /// when nothing live remains.
    fn prune_dead(&mut self, channel_id: i64, group: i64) {
        let group_emptied = match self
            .channels
            .get_mut(&channel_id)
            .and_then(|groups| groups.get_mut(&group))
        {
            Some(aliases) => {
                let mut emptied_any = false;
                for conns in aliases.values_mut() {
                    let before = conns.len();
                    conns.retain(|_, recipient| recipient.connected());
                    if before != 0 && conns.is_empty() {
                        emptied_any = true;
                    }
                }
                aliases.retain(|_, conns| !conns.is_empty());
                emptied_any && aliases.is_empty()
            }
            None => false,
        };
        if group_emptied && let Some(groups) = self.channels.get_mut(&channel_id) {
            groups.remove(&group);
            if groups.is_empty() {
                self.channels.remove(&channel_id);
            }
        }
    }

    /// Remove `conn_key` from a channel group's connection sets, returning
    /// whether that was the group's last connection anywhere in the channel
    /// (the participant group entry was removed entirely).
    fn remove_conn(&mut self, channel_id: i64, group: i64, alias_id: i64, conn_key: usize) -> bool {
        let Some(groups) = self.channels.get_mut(&channel_id) else {
            return false;
        };
        let Some(aliases) = groups.get_mut(&group) else {
            return false;
        };
        let Some(conns) = aliases.get_mut(&alias_id) else {
            return false;
        };
        conns.remove(&conn_key);
        if !conns.is_empty() {
            return false;
        }
        aliases.remove(&alias_id);
        if !aliases.is_empty() {
            return false;
        }
        groups.remove(&group);
        true
    }

    /// Forget a conn's (channel, group, alias) membership in the reverse
    /// index, dropping the index entry once the conn holds no memberships.
    fn forget_conn(&mut self, channel_id: i64, group: i64, alias_id: i64, conn_key: usize) {
        if let Some(list) = self.conn_channels.get_mut(&conn_key) {
            list.retain(|&(c, g, a)| c != channel_id || g != group || a != alias_id);
            if list.is_empty() {
                self.conn_channels.remove(&conn_key);
            }
        }
    }

    /// Remove an empty channel entry, if the leave emptied it.
    fn remove_empty_channel(&mut self, channel_id: i64) {
        if self
            .channels
            .get(&channel_id)
            .is_some_and(|groups| groups.is_empty())
        {
            self.channels.remove(&channel_id);
        }
    }
}

impl Actor for ChannelHub {
    type Context = Context<Self>;
}

impl Handler<JoinChannel> for ChannelHub {
    type Result = ();

    fn handle(&mut self, msg: JoinChannel, _ctx: &mut Self::Context) {
        let group = groups::group_key(msg.alias_id, msg.parent_account_id);
        let total_before = self
            .channels
            .get(&msg.channel_id)
            .and_then(|channels| channels.get(&group))
            .map_or(0, |aliases| aliases.values().map(|conns| conns.len()).sum());
        if total_before >= MAX_CONNS_PER_MEMBER {
            self.prune_dead(msg.channel_id, group);
            tracing::warn!(
                channel_id = msg.channel_id,
                group,
                total_before,
                "channel membership cap reached; pruned dead connections"
            );
        }
        // Evaluate membership *after* pruning so a stale full entry whose
        // recipients are all dead is still announced as a fresh join.
        let was_absent = match self
            .channels
            .get(&msg.channel_id)
            .and_then(|channels| channels.get(&group))
        {
            Some(aliases) => aliases.is_empty(),
            None => true,
        };
        self.channels
            .entry(msg.channel_id)
            .or_default()
            .entry(group)
            .or_default()
            .entry(msg.alias_id)
            .or_default()
            .insert(msg.conn_key, msg.recipient);
        if !self
            .conn_channels
            .get(&msg.conn_key)
            .is_some_and(|list| list.contains(&(msg.channel_id, group, msg.alias_id)))
        {
            self.conn_channels.entry(msg.conn_key).or_default().push((
                msg.channel_id,
                group,
                msg.alias_id,
            ));
        }
        if was_absent {
            // Fan out to every recipient in the channel, including the
            // just-registered one: a fresh group announces the new member.
            self.broadcast_channel_change(
                msg.channel_id,
                &ChannelChange {
                    channel_id: msg.channel_id,
                    alias_id: msg.alias_id,
                    joined: true,
                },
            );
        }
    }
}

impl Handler<LeaveChannel> for ChannelHub {
    type Result = ();

    fn handle(&mut self, msg: LeaveChannel, _ctx: &mut Self::Context) {
        let group = groups::group_key(msg.alias_id, msg.parent_account_id);
        let removed_group = self.remove_conn(msg.channel_id, group, msg.alias_id, msg.conn_key);
        self.forget_conn(msg.channel_id, group, msg.alias_id, msg.conn_key);
        if removed_group {
            // The departed connection is removed before the fan-out, so it
            // never receives its own `player-left` envelope.
            self.remove_empty_channel(msg.channel_id);
            self.broadcast_channel_change(
                msg.channel_id,
                &ChannelChange {
                    channel_id: msg.channel_id,
                    alias_id: msg.alias_id,
                    joined: false,
                },
            );
        }
    }
}

impl Handler<ChannelMessage> for ChannelHub {
    type Result = ();

    fn handle(&mut self, msg: ChannelMessage, _ctx: &mut Self::Context) {
        // Drop dead recipients so a missed `LeaveAllChannels` cannot accumulate
        // deliveries; a channel that lost every live connection behaves like an
        // empty one and is dropped.
        let emptied = match self.channels.get_mut(&msg.channel_id) {
            Some(groups) => {
                for aliases in groups.values_mut() {
                    for conns in aliases.values_mut() {
                        conns.retain(|_, recipient| recipient.connected());
                    }
                    aliases.retain(|_, conns| !conns.is_empty());
                }
                groups.retain(|_, aliases| !aliases.is_empty());
                groups.is_empty()
            }
            None => return,
        };
        if emptied {
            self.channels.remove(&msg.channel_id);
            return;
        }
        // Only participants may broadcast: a send from a group that is not a
        // channel participant is a silent no-op (mirrors the non-member leave
        // no-op; there is no sender-reachable error channel yet, a v0 trade-off
        // over Talo's rejection envelope).
        let sender_is_member = self
            .channels
            .get(&msg.channel_id)
            .is_some_and(|groups| groups.contains_key(&msg.group));
        if !sender_is_member {
            return;
        }
        self.broadcast_channel_message(&msg);
    }
}

impl Handler<LeaveAllChannels> for ChannelHub {
    type Result = ();

    fn handle(&mut self, msg: LeaveAllChannels, _ctx: &mut Self::Context) {
        let Some(memberships) = self.conn_channels.remove(&msg.conn_key) else {
            return;
        };
        for (channel_id, group, alias_id) in memberships {
            // Reverse index is already gone; only the channel map needs it.
            let removed_group = self.remove_conn(channel_id, group, alias_id, msg.conn_key);
            if removed_group {
                self.remove_empty_channel(channel_id);
                self.broadcast_channel_change(
                    channel_id,
                    &ChannelChange {
                        channel_id,
                        alias_id,
                        joined: false,
                    },
                );
            }
        }
    }
}

/// Build the Talo `v1.channels.player-joined` envelope for a membership.
pub fn player_joined_payload(channel_id: i64, alias_id: i64) -> Value {
    json!({
        "res": "v1.channels.player-joined",
        "data": {
            "channel": { "id": channel_id },
            "playerAlias": { "id": alias_id },
        },
    })
}

/// Build the Talo `v1.channels.player-left` envelope for a membership leave.
pub fn player_left_payload(channel_id: i64, alias_id: i64) -> Value {
    json!({
        "res": "v1.channels.player-left",
        "data": {
            "channel": { "id": channel_id },
            "playerAlias": { "id": alias_id },
            "meta": { "reason": PLAYER_LEFT_REASON_DEFAULT },
        },
    })
}

/// Build the Talo `v1.channels.message` envelope fanned out to channel members
/// when a participant sends a chat message. `message` is a plain string (the
/// 0.3.10-era echo shape with `{id, from, message}` is dropped and the sender's
/// `playerAlias` is carried instead, matching the verified upstream fan-out).
pub fn channel_message_payload(channel_id: i64, alias_id: i64, message: &str) -> Value {
    json!({
        "res": "v1.channels.message",
        "data": {
            "channel": { "id": channel_id },
            "message": message,
            "playerAlias": { "id": alias_id },
        },
    })
}

/// Process-global channel hub shared by every connection in this process.
///
/// The actor is started lazily on first use, so callers may only touch this
/// from within a running actix [`System`][actix::System] (always true for code
/// reached during request handling or integration tests).
pub fn hub() -> Addr<ChannelHub> {
    static HUB: Lazy<Addr<ChannelHub>> = Lazy::new(|| ChannelHub::new().start());
    HUB.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// A fan-out event a mock subscriber records.
    #[derive(Debug, PartialEq, Eq, Clone)]
    enum ChannelEvent {
        /// A membership transition (`joined` bool last).
        Change(i64, i64, bool),
        /// A chat message broadcast (channel, sender alias, text).
        Message(i64, i64, String),
    }

    /// Event list shared between a mock subscriber and the test body.
    type ChannelEvents = Arc<Mutex<Vec<ChannelEvent>>>;

    /// Test subscriber recording every channel notification it receives.
    struct MockChannelSub {
        events: ChannelEvents,
    }

    impl Actor for MockChannelSub {
        type Context = Context<Self>;
    }

    impl Handler<ChannelNotification> for MockChannelSub {
        type Result = ();
        fn handle(&mut self, msg: ChannelNotification, _ctx: &mut Context<Self>) {
            let event = match msg {
                ChannelNotification::Change(change) => {
                    ChannelEvent::Change(change.channel_id, change.alias_id, change.joined)
                }
                ChannelNotification::Message(message) => {
                    ChannelEvent::Message(message.channel_id, message.alias_id, message.message)
                }
            };
            self.events.lock().unwrap().push(event);
        }
    }

    /// Stop message so a test can fabricate a "dead" channel recipient.
    struct StopChannelSub;

    impl Message for StopChannelSub {
        type Result = ();
    }

    impl Handler<StopChannelSub> for MockChannelSub {
        type Result = ();
        fn handle(&mut self, _msg: StopChannelSub, ctx: &mut Context<Self>) {
            ctx.stop();
        }
    }

    fn mock() -> (ChannelEvents, Recipient<ChannelNotification>) {
        let events: ChannelEvents = Arc::new(Mutex::new(Vec::new()));
        let recipient = MockChannelSub {
            events: events.clone(),
        }
        .start()
        .recipient();
        (events, recipient)
    }

    /// Poll a subscriber until it has recorded `len` events (or fail).
    async fn wait_events(events: &ChannelEvents, len: usize) -> Vec<ChannelEvent> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if events.lock().unwrap().len() >= len {
                return events.lock().unwrap().clone();
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {len} channel events; got {:?}",
                events.lock().unwrap().clone()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[actix::test]
    async fn first_member_join_broadcasts_joined_to_all() {
        let hub = ChannelHub::new().start();
        let (e1, r1) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: r1,
        })
        .await
        .unwrap();
        let got = wait_events(&e1, 1).await;
        assert_eq!(got, vec![ChannelEvent::Change(5, 1, true)]);
    }

    #[actix::test]
    async fn second_conn_same_alias_joins_silently() {
        let hub = ChannelHub::new().start();
        let (a_events, a) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: a,
        })
        .await
        .unwrap();
        wait_events(&a_events, 1).await;

        // An alias's second connection joins without re-broadcasting `joined`
        // (group-level idempotency); the new conn sees nothing either.
        let (b_events, b) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 11,
            recipient: b,
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            a_events.lock().unwrap().len(),
            1,
            "existing conn must not see a re-join broadcast"
        );
        assert!(
            b_events.lock().unwrap().is_empty(),
            "joining an already-present group must not announce"
        );

        // But the second conn still receives future broadcasts.
        let (c_events, c) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
            parent_account_id: None,
            conn_key: 12,
            recipient: c,
        })
        .await
        .unwrap();
        let got = wait_events(&b_events, 1).await;
        assert_eq!(got, vec![ChannelEvent::Change(5, 2, true)]);
        assert_eq!(
            wait_events(&a_events, 2).await,
            vec![
                ChannelEvent::Change(5, 1, true),
                ChannelEvent::Change(5, 2, true)
            ]
        );
        let _ = c_events;
    }

    #[actix::test]
    async fn second_alias_join_broadcasts() {
        let hub = ChannelHub::new().start();
        let (a_events, a) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: a,
        })
        .await
        .unwrap();
        wait_events(&a_events, 1).await;

        // Another alias joining notifies existing members and itself.
        let (b_events, b) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
            parent_account_id: None,
            conn_key: 20,
            recipient: b,
        })
        .await
        .unwrap();
        let got = wait_events(&a_events, 2).await;
        assert_eq!(got[1], ChannelEvent::Change(5, 2, true));
        assert_eq!(
            wait_events(&b_events, 1).await,
            vec![ChannelEvent::Change(5, 2, true)]
        );
    }

    #[actix::test]
    async fn leave_last_conn_broadcasts_left_to_remaining() {
        let hub = ChannelHub::new().start();
        let (leaver_events, leaver) = mock();
        let (remaining_events, remaining) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: leaver,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
            parent_account_id: None,
            conn_key: 20,
            recipient: remaining,
        })
        .await
        .unwrap();
        // The leaver subscribed at join time (sees both joins); the survivor
        // only subscribed when its own alias joined (sees its own join).
        wait_events(&leaver_events, 2).await;
        wait_events(&remaining_events, 1).await;

        hub.send(LeaveChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
        })
        .await
        .unwrap();
        // Survivor sees the leave; the departed conn sees nothing (it was
        // removed before the fan-out, so it never gets its own `player-left`).
        let got = wait_events(&remaining_events, 2).await;
        assert_eq!(got[1], ChannelEvent::Change(5, 1, false));
        assert_eq!(
            *leaver_events.lock().unwrap(),
            vec![
                ChannelEvent::Change(5, 1, true),
                ChannelEvent::Change(5, 2, true)
            ],
            "departed recipient must not receive its own leave broadcast"
        );
    }

    #[actix::test]
    async fn leave_with_surviving_conn_is_noop() {
        let hub = ChannelHub::new().start();
        // Alias 1 holds two connections; alias 2 subscribes only when its own
        // alias joins, so it sees exactly one broadcast: its own join.
        let (a1_events, a1) = mock();
        let (a2_events, a2) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: a1.clone(),
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 11,
            recipient: a1,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
            parent_account_id: None,
            conn_key: 20,
            recipient: a2,
        })
        .await
        .unwrap();
        wait_events(&a1_events, 2).await;
        wait_events(&a2_events, 1).await;

        // One of two connections leaves: alias still in the channel -> noop.
        let before = a2_events.lock().unwrap().len();
        hub.send(LeaveChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            a2_events.lock().unwrap().len(),
            before,
            "leave with a surviving conn must not broadcast"
        );
        // The mock recipient backs both of alias 1's conns, so the alias 2
        // join broadcast reaches it under both conn keys; alias 1 is still a
        // member after one of its two conns leaves.
        assert_eq!(
            *a1_events.lock().unwrap(),
            vec![
                ChannelEvent::Change(5, 1, true),
                ChannelEvent::Change(5, 2, true),
                ChannelEvent::Change(5, 2, true),
            ],
            "alias 1 must still be a member after one of two conns leaves"
        );
    }

    #[actix::test]
    async fn leave_non_member_is_noop() {
        let hub = ChannelHub::new().start();
        let (sub_events, sub) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: sub,
        })
        .await
        .unwrap();
        wait_events(&sub_events, 1).await;

        // Unknown channel, unknown alias, already-left conn: all no-ops.
        hub.send(LeaveChannel {
            channel_id: 99,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
        })
        .await
        .unwrap();
        hub.send(LeaveChannel {
            channel_id: 5,
            alias_id: 99,
            parent_account_id: None,
            conn_key: 10,
        })
        .await
        .unwrap();
        hub.send(LeaveAllChannels { conn_key: 99 }).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            *sub_events.lock().unwrap(),
            vec![ChannelEvent::Change(5, 1, true)],
            "non-member leaves must produce no events"
        );
    }

    #[actix::test]
    async fn leave_all_on_disconnect_fans_per_channel() {
        let hub = ChannelHub::new().start();
        // Alias 1 (conn 10) joins channel 5 and channel 9; observers in each.
        let (chan5_events, chan5) = mock();
        let (chan9_events, chan9) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: chan5.clone(),
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 9,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: chan9.clone(),
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
            parent_account_id: None,
            conn_key: 20,
            recipient: chan5.clone(),
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 9,
            alias_id: 3,
            parent_account_id: None,
            conn_key: 30,
            recipient: chan9.clone(),
        })
        .await
        .unwrap();
        wait_events(&chan5_events, 3).await;
        wait_events(&chan9_events, 3).await;

        // The dropped connection leaves both channels; each channel's
        // surviving member sees its own `player-left` (group's last conn).
        hub.send(LeaveAllChannels { conn_key: 10 }).await.unwrap();
        let chan5_got = wait_events(&chan5_events, 4).await;
        assert_eq!(chan5_got[3], ChannelEvent::Change(5, 1, false));
        let chan9_got = wait_events(&chan9_events, 4).await;
        assert_eq!(chan9_got[3], ChannelEvent::Change(9, 1, false));
    }

    // ---- Task 0.3.14: subaccount participant (group) semantics ----

    #[actix::test]
    async fn subaccount_join_announces_once_per_group() {
        let hub = ChannelHub::new().start();
        let (parent_events, parent) = mock();
        let (sub_events, sub) = mock();
        // Root account alias 100 joins first: its group (100) is absent.
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 100,
            parent_account_id: None,
            conn_key: 10,
            recipient: parent,
        })
        .await
        .unwrap();
        wait_events(&parent_events, 1).await;
        // A subaccount (alias 50, parent 100) joins the same group: silent.
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 50,
            parent_account_id: Some(100),
            conn_key: 20,
            recipient: sub,
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            *parent_events.lock().unwrap(),
            vec![ChannelEvent::Change(5, 100, true)],
            "group-level idempotency: a second conn in an existing group must not re-announce"
        );
        assert!(
            sub_events.lock().unwrap().is_empty(),
            "joining an already-present group must not announce to the new conn"
        );
    }

    #[actix::test]
    async fn subaccount_and_parent_share_chat() {
        let hub = ChannelHub::new().start();
        let (parent_events, parent) = mock();
        let (sub_events, sub) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 100,
            parent_account_id: None,
            conn_key: 10,
            recipient: parent,
        })
        .await
        .unwrap();
        wait_events(&parent_events, 1).await;
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 50,
            parent_account_id: Some(100),
            conn_key: 20,
            recipient: sub,
        })
        .await
        .unwrap();

        // A message from the subaccount lands on the parent conn and its own;
        // a message from the parent lands on the subaccount conn and its own.
        hub.send(ChannelMessage {
            channel_id: 5,
            alias_id: 50,
            group: 100,
            message: "hi from sub".to_string(),
        })
        .await
        .unwrap();
        hub.send(ChannelMessage {
            channel_id: 5,
            alias_id: 100,
            group: 100,
            message: "hi from parent".to_string(),
        })
        .await
        .unwrap();

        assert_eq!(
            wait_events(&parent_events, 3).await,
            vec![
                ChannelEvent::Change(5, 100, true),
                ChannelEvent::Message(5, 50, "hi from sub".to_string()),
                ChannelEvent::Message(5, 100, "hi from parent".to_string()),
            ],
            "parent conn must receive the subaccount's message and its own"
        );
        assert_eq!(
            wait_events(&sub_events, 2).await,
            vec![
                ChannelEvent::Message(5, 50, "hi from sub".to_string()),
                ChannelEvent::Message(5, 100, "hi from parent".to_string()),
            ],
            "subaccount conn must receive both messages exactly once each"
        );
    }

    #[actix::test]
    async fn distinct_parents_are_distinct_participants() {
        let hub = ChannelHub::new().start();
        // Two subaccounts of different parents: two distinct participant groups.
        let (a_events, a) = mock(); // alias 50, group 100
        let (b_events, b) = mock(); // alias 60, group 200
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 50,
            parent_account_id: Some(100),
            conn_key: 10,
            recipient: a,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 60,
            parent_account_id: Some(200),
            conn_key: 20,
            recipient: b,
        })
        .await
        .unwrap();
        assert_eq!(
            wait_events(&a_events, 2).await,
            vec![
                ChannelEvent::Change(5, 50, true),
                ChannelEvent::Change(5, 60, true),
            ],
            "two distinct parent groups announce two separate joins"
        );
        // `b` joined second, so it never saw the group 100 announcement (which
        // predates its subscription) — only its own group 200 join.
        assert_eq!(
            wait_events(&b_events, 1).await,
            vec![ChannelEvent::Change(5, 60, true)]
        );

        // Leaving group 100's only conn announces a leave for alias 50 to the
        // surviving participant; group 200 stays a member. The departed conn
        // is removed before the fan-out, so `a` never sees its own leave.
        hub.send(LeaveChannel {
            channel_id: 5,
            alias_id: 50,
            parent_account_id: Some(100),
            conn_key: 10,
        })
        .await
        .unwrap();
        let got = wait_events(&b_events, 2).await;
        assert_eq!(got[1], ChannelEvent::Change(5, 50, false));
        assert_eq!(
            *a_events.lock().unwrap(),
            vec![
                ChannelEvent::Change(5, 50, true),
                ChannelEvent::Change(5, 60, true),
            ],
            "the departed conn must not receive its own leave broadcast"
        );
    }

    #[actix::test]
    async fn group_level_leave_on_last_conn_of_group() {
        let hub = ChannelHub::new().start();
        // Group 100 holds a parent conn and a subaccount conn; observer 70 is a
        // separate participant group that stays to watch the fan-out.
        let (parent_events, parent) = mock();
        let (observer_events, observer) = mock();
        let (sub_events, sub) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 100,
            parent_account_id: None,
            conn_key: 10,
            recipient: parent,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 70,
            parent_account_id: None,
            conn_key: 30,
            recipient: observer,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 50,
            parent_account_id: Some(100),
            conn_key: 20,
            recipient: sub,
        })
        .await
        .unwrap();
        // Joins: group 100 (alias 100), then group 70 (alias 70) — the observer
        // sees only its own join, the parent sees both; the subaccount joins
        // the already-present group 100 (silent).
        wait_events(&parent_events, 2).await;
        wait_events(&observer_events, 1).await;

        // Parent leaves: group 100 still has the subaccount's conn -> noop.
        hub.send(LeaveChannel {
            channel_id: 5,
            alias_id: 100,
            parent_account_id: None,
            conn_key: 10,
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            observer_events.lock().unwrap().len(),
            1,
            "a leave with a surviving group conn must not broadcast"
        );

        // Subaccount leaves: group 100 has no live conns left -> `player-left`
        // carries the departing alias 50, observed by the surviving group.
        hub.send(LeaveChannel {
            channel_id: 5,
            alias_id: 50,
            parent_account_id: Some(100),
            conn_key: 20,
        })
        .await
        .unwrap();
        let got = wait_events(&observer_events, 2).await;
        assert_eq!(
            got[1],
            ChannelEvent::Change(5, 50, false),
            "player-left carries the departing alias of the group's last conn"
        );
        assert!(
            sub_events.lock().unwrap().is_empty(),
            "the departing subaccount conn must never receive its own leave broadcast"
        );
    }

    #[actix::test]
    async fn subaccount_send_to_own_group_members_ok_check() {
        let hub = ChannelHub::new().start();
        let (parent_events, parent) = mock();
        let (sub_events, sub) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 100,
            parent_account_id: None,
            conn_key: 10,
            recipient: parent,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 50,
            parent_account_id: Some(100),
            conn_key: 20,
            recipient: sub,
        })
        .await
        .unwrap();
        wait_events(&parent_events, 1).await;

        // A subaccount send from a member group broadcasts to every conn in the
        // channel (group-level membership gate), parent and sender included.
        hub.send(ChannelMessage {
            channel_id: 5,
            alias_id: 50,
            group: 100,
            message: "hi".to_string(),
        })
        .await
        .unwrap();
        let got_parent = wait_events(&parent_events, 2).await;
        assert_eq!(
            got_parent[1],
            ChannelEvent::Message(5, 50, "hi".to_string())
        );
        assert_eq!(
            wait_events(&sub_events, 1).await,
            vec![ChannelEvent::Message(5, 50, "hi".to_string())],
            "the sender's own conn must receive its message too"
        );

        // A send from a group that is NOT a participant stays a silent no-op.
        let before = parent_events.lock().unwrap().len();
        hub.send(ChannelMessage {
            channel_id: 5,
            alias_id: 77,
            group: 999,
            message: "nobody".to_string(),
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            parent_events.lock().unwrap().len(),
            before,
            "a non-participant group's send must be a silent no-op"
        );
    }

    #[actix::test]
    async fn broadcast_message_fans_out_to_all_members() {
        let hub = ChannelHub::new().start();
        let (e1, r1) = mock();
        let (e2, r2) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: r1,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
            parent_account_id: None,
            conn_key: 20,
            recipient: r2,
        })
        .await
        .unwrap();
        wait_events(&e1, 2).await;
        wait_events(&e2, 1).await;

        hub.send(ChannelMessage {
            channel_id: 5,
            alias_id: 1,
            group: 1,
            message: "hi".to_string(),
        })
        .await
        .unwrap();
        wait_events(&e1, 3).await;
        wait_events(&e2, 2).await;
        assert_eq!(
            *e1.lock().unwrap(),
            vec![
                ChannelEvent::Change(5, 1, true),
                ChannelEvent::Change(5, 2, true),
                ChannelEvent::Message(5, 1, "hi".to_string()),
            ],
            "the sender's own connection must receive its message too"
        );
        assert_eq!(
            *e2.lock().unwrap(),
            vec![
                ChannelEvent::Change(5, 2, true),
                ChannelEvent::Message(5, 1, "hi".to_string()),
            ]
        );
    }

    #[actix::test]
    async fn broadcast_message_reaches_each_conn_once() {
        let hub = ChannelHub::new().start();
        // Alias 1 holds two connections, each with its own subscriber; alias 2
        // is a second member. The alias 2 join fan-out reaches both of alias
        // 1's conns; a message from alias 1 must also reach each conn exactly
        // once (one envelope per conn key, never a merged or double delivery).
        let (a1c1_events, a1c1) = mock();
        let (a1c2_events, a1c2) = mock();
        let (a2_events, a2) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: a1c1,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 11,
            recipient: a1c2,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
            parent_account_id: None,
            conn_key: 20,
            recipient: a2,
        })
        .await
        .unwrap();
        // conn 10 saw its own alias's join + alias 2's join; conn 11 (joined an
        // already-present group silently) and alias 2 only saw alias 2's join.
        wait_events(&a1c1_events, 2).await;
        wait_events(&a1c2_events, 1).await;
        wait_events(&a2_events, 1).await;

        hub.send(ChannelMessage {
            channel_id: 5,
            alias_id: 1,
            group: 1,
            message: "hey".to_string(),
        })
        .await
        .unwrap();
        wait_events(&a1c1_events, 3).await;
        wait_events(&a1c2_events, 2).await;
        wait_events(&a2_events, 2).await;
        assert_eq!(
            *a1c1_events.lock().unwrap(),
            vec![
                ChannelEvent::Change(5, 1, true),
                ChannelEvent::Change(5, 2, true),
                ChannelEvent::Message(5, 1, "hey".to_string()),
            ],
            "alias 1 conn 10 must receive the message once"
        );
        assert_eq!(
            *a1c2_events.lock().unwrap(),
            vec![
                ChannelEvent::Change(5, 2, true),
                ChannelEvent::Message(5, 1, "hey".to_string()),
            ],
            "alias 1 conn 11 must receive the message once"
        );
        assert_eq!(
            *a2_events.lock().unwrap(),
            vec![
                ChannelEvent::Change(5, 2, true),
                ChannelEvent::Message(5, 1, "hey".to_string()),
            ]
        );
    }

    #[actix::test]
    async fn broadcast_message_unknown_channel_is_noop() {
        let hub = ChannelHub::new().start();
        let (sub_events, sub) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: sub,
        })
        .await
        .unwrap();
        wait_events(&sub_events, 1).await;

        hub.send(ChannelMessage {
            channel_id: 99,
            alias_id: 1,
            group: 1,
            message: "hi".to_string(),
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            *sub_events.lock().unwrap(),
            vec![ChannelEvent::Change(5, 1, true)],
            "sending into an unknown channel must produce no events"
        );
    }

    #[actix::test]
    async fn broadcast_message_non_member_send_is_noop() {
        let hub = ChannelHub::new().start();
        let (sub_events, sub) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            parent_account_id: None,
            conn_key: 10,
            recipient: sub,
        })
        .await
        .unwrap();
        wait_events(&sub_events, 1).await;

        // Group 99 is not a participant of channel 5; its message reaches nobody.
        hub.send(ChannelMessage {
            channel_id: 5,
            alias_id: 99,
            group: 99,
            message: "hi".to_string(),
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            *sub_events.lock().unwrap(),
            vec![ChannelEvent::Change(5, 1, true)],
            "a non-member group send must be a silent no-op"
        );
    }

    /// Load test: 100 connections, 1000 membership broadcasts, 0 dropped
    /// envelopes. 100 members (root accounts, one group each) register in one
    /// channel (setup fans them a cumulative 5050 `joined` events), then a
    /// traveling *subaccount* (alias 50_000, parent 60_000) joins/leaves 5
    /// times; each transition fans 100 `joined`/`left` events out to the 100
    /// member groups (1000 total). The exact counts landing prove no envelope
    /// is dropped or double-delivered.
    #[actix::test]
    async fn load_100_conns_1000_broadcasts_0_drops() {
        let hub = ChannelHub::new().start();
        const CONNS: usize = 100;
        const CYCLES: usize = 5;
        const LEAVER_ALIAS: i64 = 50_000;
        const LEAVER_PARENT: i64 = 60_000;
        const LEAVER_CONN: usize = 999;

        let (member_events, member_recv) = mock();
        let (leaver_events, leaver_recv) = mock();

        for i in 0..CONNS {
            hub.send(JoinChannel {
                channel_id: 5,
                alias_id: i as i64 + 1,
                parent_account_id: None,
                conn_key: i,
                recipient: member_recv.clone(),
            })
            .await
            .unwrap();
        }

        for _ in 0..CYCLES {
            hub.send(JoinChannel {
                channel_id: 5,
                alias_id: LEAVER_ALIAS,
                parent_account_id: Some(LEAVER_PARENT),
                conn_key: LEAVER_CONN,
                recipient: leaver_recv.clone(),
            })
            .await
            .unwrap();
            hub.send(LeaveChannel {
                channel_id: 5,
                alias_id: LEAVER_ALIAS,
                parent_account_id: Some(LEAVER_PARENT),
                conn_key: LEAVER_CONN,
            })
            .await
            .unwrap();
        }

        // Setup fan-out is cumulative: the k-th group join reaches k members.
        // Each traveling cycle then delivers CONNS `joined` + CONNS `left` to
        // the 100 members (1000 across CYCLES); the leaver only ever sees its
        // own join broadcast, never its leave (removed before the fan-out).
        let expected_member = (1..=CONNS).sum::<usize>() + CYCLES * (CONNS * 2);
        let expected_leaver = CYCLES;

        wait_events(&member_events, expected_member).await;
        wait_events(&leaver_events, expected_leaver).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(
            member_events.lock().unwrap().len(),
            expected_member,
            "100 conns, {} broadcasts, 0 dropped envelopes",
            CYCLES * CONNS * 2
        );
        assert_eq!(
            leaver_events.lock().unwrap().len(),
            expected_leaver,
            "leaver must receive exactly one join per cycle and never its own leave"
        );
    }

    /// Load test: 100 member connections, 1000 chat broadcasts, 0 dropped or
    /// double-delivered envelopes. 100 members register in one channel
    /// (cumulative 5050 `joined` events), then member alias 1 — a subaccount
    /// grouped under parent 900 — sends 1000 chat messages; each fans out to
    /// all 100 member conn keys, sender included (100k deliveries). The exact
    /// final count proves nothing was dropped or delivered twice.
    #[actix::test]
    async fn load_100_conns_1000_chat_broadcasts_0_drops() {
        let hub = ChannelHub::new().start();
        const CONNS: usize = 100;
        const MSGS: usize = 1000;
        const SENDER_PARENT: i64 = 900;

        let (member_events, member_recv) = mock();

        for i in 0..CONNS {
            // Alias 1 joins as a subaccount of parent 900; the rest are roots.
            let (alias, parent) = if i == 0 {
                (1i64, Some(SENDER_PARENT))
            } else {
                (i as i64 + 1, None)
            };
            hub.send(JoinChannel {
                channel_id: 5,
                alias_id: alias,
                parent_account_id: parent,
                conn_key: i,
                recipient: member_recv.clone(),
            })
            .await
            .unwrap();
        }

        // Setup fan-out is cumulative: the k-th group's join reaches k members.
        let setup_events = (1..=CONNS).sum::<usize>();

        for _ in 0..MSGS {
            hub.send(ChannelMessage {
                channel_id: 5,
                alias_id: 1,
                group: SENDER_PARENT,
                message: "hi".to_string(),
            })
            .await
            .unwrap();
        }

        let expected = setup_events + MSGS * CONNS;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if member_events.lock().unwrap().len() >= expected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected} chat events; got {:?}",
                member_events.lock().unwrap().len()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!(
            member_events.lock().unwrap().len(),
            expected,
            "100 member conns, {MSGS} chat broadcasts, 0 dropped or double-delivered envelopes"
        );
    }

    #[actix::test]
    async fn channel_message_payload_shape() {
        let payload = channel_message_payload(5, 1, "hi");
        assert_eq!(payload["res"], "v1.channels.message");
        assert_eq!(payload["data"]["channel"]["id"].as_i64(), Some(5));
        assert_eq!(
            payload["data"]["message"].as_str(),
            Some("hi"),
            "message must be a plain string, not an object"
        );
        assert_eq!(payload["data"]["playerAlias"]["id"].as_i64(), Some(1));
    }

    #[actix::test]
    async fn player_joined_payload_shape() {
        let payload = player_joined_payload(5, 1);
        assert_eq!(payload["res"], "v1.channels.player-joined");
        assert_eq!(payload["data"]["channel"]["id"].as_i64(), Some(5));
        assert_eq!(payload["data"]["playerAlias"]["id"].as_i64(), Some(1));
    }

    #[actix::test]
    async fn player_left_payload_shape() {
        let payload = player_left_payload(5, 1);
        assert_eq!(payload["res"], "v1.channels.player-left");
        assert_eq!(payload["data"]["channel"]["id"].as_i64(), Some(5));
        assert_eq!(payload["data"]["playerAlias"]["id"].as_i64(), Some(1));
        assert_eq!(
            payload["data"]["meta"]["reason"].as_i64(),
            Some(0),
            "leaving reason must serialize as a number (TS numeric enum), not a string"
        );
        assert!(payload["data"]["meta"]["reason"].is_number());
    }
}
