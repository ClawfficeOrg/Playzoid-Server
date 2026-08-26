//! In-memory WebSocket game-channel membership registry.
//!
//! Tracks which player aliases are members of which channels and broadcasts
//! the Talo `v1.channels.player-joined` / `v1.channels.player-left` envelopes
//! to member connections when membership changes. A player joins when its
//! first connection registers for a channel and leaves when its last
//! connection drops, mirroring the presence hub's online/offline semantics.
//!
//! Upstream Talo drives channel membership over HTTP and only fans out the
//! membership changes over sockets; `v1.channels.join` / `v1.channels.leave`
//! are Playzoid request extensions (the only in-scope trigger while no
//! channel persistence exists). The response envelopes stay Talo-verified.

use actix::prelude::*;
use once_cell::sync::Lazy;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Upper bound of live connections tracked per (channel, alias) membership.
/// Guards against unbounded memory growth if a recipient's `stopping()`-issued
/// leave is ever missed; exceeding it triggers a prune of dead recipients
/// instead of failing the join.
const MAX_CONNS_PER_MEMBER: usize = 256;

/// Numeric `GameChannelLeavingReason::DEFAULT` (serialized as an integer, per
/// the upstream TS numeric enum).
const PLAYER_LEFT_REASON_DEFAULT: i64 = 0;

/// A channel membership transition: `joined: true` when the first connection
/// for an alias registers in a channel, `joined: false` when the last
/// connection drops.
#[derive(Message, Clone)]
#[rtype(result = "()")]
pub struct ChannelChange {
    /// Channel the transition belongs to.
    pub channel_id: i64,
    /// Player alias the transition belongs to.
    pub alias_id: i64,
    /// `true` when the player joined the channel, `false` when it left.
    pub joined: bool,
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
    /// Unique connection key so the same connection can be unregistered later.
    pub conn_key: usize,
    /// Recipient to push subsequent channel changes to.
    pub recipient: Recipient<ChannelChange>,
}

/// Remove a connection from a single channel.
#[derive(Message)]
#[rtype(result = "()")]
pub struct LeaveChannel {
    /// Channel the connection is leaving.
    pub channel_id: i64,
    /// Player alias the connection was registered under.
    pub alias_id: i64,
    /// Unique connection key of the departing connection.
    pub conn_key: usize,
}

/// Remove a connection from every channel it joined.
#[derive(Message)]
#[rtype(result = "()")]
pub struct LeaveAllChannels {
    /// Player alias the connection was registered under.
    pub alias_id: i64,
    /// Unique connection key of the departing connection.
    pub conn_key: usize,
}

/// Connection registry keyed by channel id, then alias id, then conn key.
type ChannelMemberships = HashMap<i64, HashMap<i64, HashMap<usize, Recipient<ChannelChange>>>>;

/// Reverse index mapping a connection key to the memberships it holds, so a
/// dropped connection can be unregistered in O(its channels) not O(all).
type ConnMemberships = HashMap<usize, Vec<(i64, i64)>>;

/// Actor owning the channel membership registry.
#[derive(Default)]
pub struct ChannelHub {
    /// Live memberships keyed by channel, then alias, then connection key.
    channels: ChannelMemberships,
    /// Reverse index: connection key -> (channel, alias) pairs it joined.
    conn_channels: ConnMemberships,
}

impl ChannelHub {
    /// Create an empty channel hub.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fan a channel change out to every connection currently in the channel.
    fn broadcast_channel_change(&self, channel_id: i64, change: &ChannelChange) {
        if let Some(aliases) = self.channels.get(&channel_id) {
            for set in aliases.values() {
                for recipient in set.values() {
                    recipient.do_send(change.clone());
                }
            }
        }
    }

    /// Drop recipients whose underlying actor has stopped, removing the alias
    /// entry entirely when no connection remains.
    fn prune_dead(&mut self, channel_id: i64, alias_id: i64) {
        let emptied = match self
            .channels
            .get_mut(&channel_id)
            .and_then(|aliases| aliases.get_mut(&alias_id))
        {
            Some(set) => {
                let before = set.len();
                set.retain(|_, recipient| recipient.connected());
                before != 0 && set.is_empty()
            }
            None => false,
        };
        if emptied && let Some(aliases) = self.channels.get_mut(&channel_id) {
            aliases.remove(&alias_id);
        }
    }

    /// Remove `conn_key` from a channel alias's connection set, returning
    /// whether that was the alias's last connection (alias entry removed).
    fn remove_conn(&mut self, channel_id: i64, alias_id: i64, conn_key: usize) -> bool {
        match self.channels.get_mut(&channel_id) {
            Some(aliases) => match aliases.get_mut(&alias_id) {
                Some(conns) => {
                    conns.remove(&conn_key);
                    if conns.is_empty() {
                        aliases.remove(&alias_id);
                        true
                    } else {
                        false
                    }
                }
                None => false,
            },
            None => false,
        }
    }

    /// Forget a conn's (channel, alias) membership in the reverse index,
    /// dropping the index entry once the conn holds no memberships.
    fn forget_conn(&mut self, channel_id: i64, alias_id: i64, conn_key: usize) {
        if let Some(list) = self.conn_channels.get_mut(&conn_key) {
            list.retain(|&(c, a)| c != channel_id || a != alias_id);
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
            .is_some_and(|aliases| aliases.is_empty())
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
        if self
            .channels
            .get(&msg.channel_id)
            .and_then(|aliases| aliases.get(&msg.alias_id))
            .is_some_and(|set| set.len() >= MAX_CONNS_PER_MEMBER)
        {
            let before = self
                .channels
                .get(&msg.channel_id)
                .and_then(|aliases| aliases.get(&msg.alias_id))
                .map_or(0, |set| set.len());
            self.prune_dead(msg.channel_id, msg.alias_id);
            tracing::warn!(
                channel_id = msg.channel_id,
                alias_id = msg.alias_id,
                before,
                "channel membership cap reached; pruned dead connections"
            );
        }
        // Evaluate membership *after* pruning so a stale full entry whose
        // recipients are all dead is still announced as a fresh join.
        let was_absent = match self
            .channels
            .get(&msg.channel_id)
            .and_then(|aliases| aliases.get(&msg.alias_id))
        {
            Some(set) => set.is_empty(),
            None => true,
        };
        self.channels
            .entry(msg.channel_id)
            .or_default()
            .entry(msg.alias_id)
            .or_default()
            .insert(msg.conn_key, msg.recipient);
        if !self
            .conn_channels
            .get(&msg.conn_key)
            .is_some_and(|list| list.contains(&(msg.channel_id, msg.alias_id)))
        {
            self.conn_channels
                .entry(msg.conn_key)
                .or_default()
                .push((msg.channel_id, msg.alias_id));
        }
        if was_absent {
            // Fan out to every recipient in the channel, including the
            // just-registered one: a fresh join announces the new member.
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
        let removed_alias = self.remove_conn(msg.channel_id, msg.alias_id, msg.conn_key);
        self.forget_conn(msg.channel_id, msg.alias_id, msg.conn_key);
        if removed_alias {
            // The departed connection was removed before the fan-out, so it
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

impl Handler<LeaveAllChannels> for ChannelHub {
    type Result = ();

    fn handle(&mut self, msg: LeaveAllChannels, _ctx: &mut Self::Context) {
        let Some(memberships) = self.conn_channels.remove(&msg.conn_key) else {
            return;
        };
        for (channel_id, alias_id) in memberships {
            // Reverse index is already gone; only the channel map needs it.
            let removed_alias = self.remove_conn(channel_id, alias_id, msg.conn_key);
            if removed_alias {
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

    /// Event list shared between a mock subscriber and the test body.
    type ChannelEvents = Arc<Mutex<Vec<(i64, i64, bool)>>>;

    /// Test subscriber recording every channel change it receives.
    struct MockChannelSub {
        events: ChannelEvents,
    }

    impl Actor for MockChannelSub {
        type Context = Context<Self>;
    }

    impl Handler<ChannelChange> for MockChannelSub {
        type Result = ();
        fn handle(&mut self, msg: ChannelChange, _ctx: &mut Context<Self>) {
            self.events
                .lock()
                .unwrap()
                .push((msg.channel_id, msg.alias_id, msg.joined));
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

    fn mock() -> (ChannelEvents, Recipient<ChannelChange>) {
        let events: ChannelEvents = Arc::new(Mutex::new(Vec::new()));
        let recipient = MockChannelSub {
            events: events.clone(),
        }
        .start()
        .recipient();
        (events, recipient)
    }

    /// Poll a subscriber until it has recorded `len` events (or fail).
    async fn wait_events(events: &ChannelEvents, len: usize) -> Vec<(i64, i64, bool)> {
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
            conn_key: 10,
            recipient: r1,
        })
        .await
        .unwrap();
        let got = wait_events(&e1, 1).await;
        assert_eq!(got, vec![(5, 1, true)]);
    }

    #[actix::test]
    async fn second_conn_same_alias_joins_silently() {
        let hub = ChannelHub::new().start();
        let (a_events, a) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            conn_key: 10,
            recipient: a,
        })
        .await
        .unwrap();
        wait_events(&a_events, 1).await;

        // An alias's second connection joins without re-broadcasting `joined`
        // (alias-level idempotency); the new conn sees nothing either.
        let (b_events, b) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
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
            "joining an already-member alias must not announce"
        );

        // But the second conn still receives future broadcasts.
        let (c_events, c) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
            conn_key: 12,
            recipient: c,
        })
        .await
        .unwrap();
        let got = wait_events(&b_events, 1).await;
        assert_eq!(got, vec![(5, 2, true)]);
        assert_eq!(
            wait_events(&a_events, 2).await,
            vec![(5, 1, true), (5, 2, true)]
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
            conn_key: 20,
            recipient: b,
        })
        .await
        .unwrap();
        let got = wait_events(&a_events, 2).await;
        assert_eq!(got[1], (5, 2, true));
        assert_eq!(wait_events(&b_events, 1).await, vec![(5, 2, true)]);
    }

    #[actix::test]
    async fn leave_last_conn_broadcasts_left_to_remaining() {
        let hub = ChannelHub::new().start();
        let (leaver_events, leaver) = mock();
        let (remaining_events, remaining) = mock();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            conn_key: 10,
            recipient: leaver,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
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
            conn_key: 10,
        })
        .await
        .unwrap();
        // Survivor sees the leave; the departed conn sees nothing (it was
        // removed before the fan-out, so it never gets its own `player-left`).
        let got = wait_events(&remaining_events, 2).await;
        assert_eq!(got[1], (5, 1, false));
        assert_eq!(
            *leaver_events.lock().unwrap(),
            vec![(5, 1, true), (5, 2, true)],
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
            conn_key: 10,
            recipient: a1.clone(),
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 1,
            conn_key: 11,
            recipient: a1,
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
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
            vec![(5, 1, true), (5, 2, true), (5, 2, true)],
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
            conn_key: 10,
        })
        .await
        .unwrap();
        hub.send(LeaveChannel {
            channel_id: 5,
            alias_id: 99,
            conn_key: 10,
        })
        .await
        .unwrap();
        hub.send(LeaveAllChannels {
            alias_id: 1,
            conn_key: 99,
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            *sub_events.lock().unwrap(),
            vec![(5, 1, true)],
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
            conn_key: 10,
            recipient: chan5.clone(),
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 9,
            alias_id: 1,
            conn_key: 10,
            recipient: chan9.clone(),
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: 2,
            conn_key: 20,
            recipient: chan5.clone(),
        })
        .await
        .unwrap();
        hub.send(JoinChannel {
            channel_id: 9,
            alias_id: 3,
            conn_key: 30,
            recipient: chan9.clone(),
        })
        .await
        .unwrap();
        wait_events(&chan5_events, 3).await;
        wait_events(&chan9_events, 3).await;

        // The dropped connection leaves both channels; each channel's
        // surviving member sees its own `player-left` (alias's last conn).
        hub.send(LeaveAllChannels {
            alias_id: 1,
            conn_key: 10,
        })
        .await
        .unwrap();
        let chan5_got = wait_events(&chan5_events, 4).await;
        assert_eq!(chan5_got[3], (5, 1, false));
        let chan9_got = wait_events(&chan9_events, 4).await;
        assert_eq!(chan9_got[3], (9, 1, false));
    }

    /// Load test: 100 connections, 1000 membership broadcasts, 0 dropped
    /// envelopes. 100 members register in one channel (setup fan-outs them a
    /// cumulative 5050 `joined` events), then a traveling alias joins/leaves 5
    /// times; each transition fan-outs 100 `joined`/`left` events to the 100
    /// members (1000 total). The exact counts landing prove no envelope is
    /// dropped or double-delivered.
    #[actix::test]
    async fn load_100_conns_1000_broadcasts_0_drops() {
        let hub = ChannelHub::new().start();
        const CONNS: usize = 100;
        const CYCLES: usize = 5;
        const LEAVER_ALIAS: i64 = 50_000;
        const LEAVER_CONN: usize = 999;

        let (member_events, member_recv) = mock();
        let (leaver_events, leaver_recv) = mock();

        for i in 0..CONNS {
            hub.send(JoinChannel {
                channel_id: 5,
                alias_id: i as i64 + 1,
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
                conn_key: LEAVER_CONN,
                recipient: leaver_recv.clone(),
            })
            .await
            .unwrap();
            hub.send(LeaveChannel {
                channel_id: 5,
                alias_id: LEAVER_ALIAS,
                conn_key: LEAVER_CONN,
            })
            .await
            .unwrap();
        }

        // Setup fan-out is cumulative: the k-th alias join reaches k members.
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
