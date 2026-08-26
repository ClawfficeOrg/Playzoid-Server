//! In-memory WebSocket presence registry.
//!
//! Tracks which player aliases are currently connected and broadcasts
//! `v1.players.presence.updated` envelopes to every connected socket when a
//! player comes online (first identified connection for an alias) or goes
//! offline (last connection for the alias drops).

use actix::prelude::*;
use chrono::Utc;
use once_cell::sync::Lazy;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Upper bound of live connections tracked per player alias. Guards against
/// unbounded memory growth if a recipient's `stopping()`-issued leave is ever
/// missed; exceeding it triggers a prune of dead recipients instead of
/// failing the join.
const MAX_CONNS_PER_ALIAS: usize = 256;

/// A presence transition: `online: true` when the first connection for an
/// alias appears, `online: false` when the last connection drops.
#[derive(Message, Clone)]
#[rtype(result = "()")]
pub struct PresenceChange {
    /// Player alias the transition belongs to.
    pub alias_id: i64,
    /// `true` when the player came online, `false` when it went offline.
    pub online: bool,
}

/// Register a connection under a player alias.
#[derive(Message)]
#[rtype(result = "()")]
pub struct JoinPresence {
    /// Player alias the connection belongs to (always the server-resolved
    /// socket-ticket alias, never a client-supplied id).
    pub alias_id: i64,
    /// Unique connection key so the same connection can be unregistered later.
    pub conn_key: usize,
    /// Recipient to push subsequent presence changes to.
    pub recipient: Recipient<PresenceChange>,
}

/// Unregister a connection from its player alias.
#[derive(Message)]
#[rtype(result = "()")]
pub struct LeavePresence {
    /// Player alias the connection was registered under.
    pub alias_id: i64,
    /// Unique connection key of the departing connection.
    pub conn_key: usize,
}

/// Actor owning the presence registry.
#[derive(Default)]
pub struct PresenceHub {
    /// Registered connections keyed by player alias, then by connection key.
    conns: HashMap<i64, HashMap<usize, Recipient<PresenceChange>>>,
}

impl PresenceHub {
    /// Create an empty presence hub.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fan a presence change out to every registered connection.
    fn broadcast(&self, change: &PresenceChange) {
        for set in self.conns.values() {
            for recipient in set.values() {
                recipient.do_send(change.clone());
            }
        }
    }

    /// Drop recipients whose underlying actor has stopped, removing the alias
    /// entry entirely when no connection remains.
    fn prune_dead(&mut self, alias_id: i64) {
        let emptied = match self.conns.get_mut(&alias_id) {
            Some(set) => {
                let before = set.len();
                set.retain(|_, recipient| recipient.connected());
                before != 0 && set.is_empty()
            }
            None => false,
        };
        if emptied {
            self.conns.remove(&alias_id);
        }
    }
}

impl Actor for PresenceHub {
    type Context = Context<Self>;
}

impl Handler<JoinPresence> for PresenceHub {
    type Result = ();

    fn handle(&mut self, msg: JoinPresence, _ctx: &mut Self::Context) {
        if self
            .conns
            .get(&msg.alias_id)
            .is_some_and(|set| set.len() >= MAX_CONNS_PER_ALIAS)
        {
            let before = self.conns.get(&msg.alias_id).map_or(0, |set| set.len());
            self.prune_dead(msg.alias_id);
            tracing::warn!(
                alias_id = msg.alias_id,
                before,
                "presence recipient cap reached; pruned dead connections"
            );
        }
        // Evaluate offline-ness *after* pruning so a stale full entry whose
        // recipients are all dead is still treated as offline and re-announced.
        let was_offline = match self.conns.get(&msg.alias_id) {
            Some(set) => set.is_empty(),
            None => true,
        };
        self.conns
            .entry(msg.alias_id)
            .or_default()
            .insert(msg.conn_key, msg.recipient);
        if was_offline {
            self.broadcast(&PresenceChange {
                alias_id: msg.alias_id,
                online: true,
            });
        }
    }
}

impl Handler<LeavePresence> for PresenceHub {
    type Result = ();

    fn handle(&mut self, msg: LeavePresence, _ctx: &mut Self::Context) {
        let went_offline = match self.conns.get_mut(&msg.alias_id) {
            Some(set) => {
                set.remove(&msg.conn_key);
                if set.is_empty() {
                    self.conns.remove(&msg.alias_id);
                    true
                } else {
                    false
                }
            }
            None => false,
        };
        if went_offline {
            self.broadcast(&PresenceChange {
                alias_id: msg.alias_id,
                online: false,
            });
        }
    }
}

/// Build the Talo `v1.players.presence.updated` envelope for a transition.
pub fn presence_payload(alias_id: i64, online: bool) -> Value {
    json!({
        "res": "v1.players.presence.updated",
        "data": {
            "presence": {
                "playerAlias": { "id": alias_id },
                "online": online,
                "customStatus": null,
                "lastSeenAt": Utc::now().to_rfc3339(),
            },
            "meta": {
                "onlineChanged": true,
                "customStatusChanged": false,
            },
        },
    })
}

/// Process-global presence hub shared by every connection in this process.
///
/// The actor is started lazily on first use, so callers may only touch this
/// from within a running actix [`System`][actix::System] (always true for code
/// reached during request handling or integration tests).
pub fn hub() -> Addr<PresenceHub> {
    static HUB: Lazy<Addr<PresenceHub>> = Lazy::new(|| PresenceHub::new().start());
    HUB.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Event list shared between a mock subscriber and the test body.
    type PresenceEvents = Arc<Mutex<Vec<(i64, bool)>>>;

    /// Test subscriber recording every presence change it receives.
    struct MockPresenceSub {
        events: PresenceEvents,
    }

    impl Actor for MockPresenceSub {
        type Context = Context<Self>;
    }

    impl Handler<PresenceChange> for MockPresenceSub {
        type Result = ();
        fn handle(&mut self, msg: PresenceChange, _ctx: &mut Context<Self>) {
            self.events.lock().unwrap().push((msg.alias_id, msg.online));
        }
    }

    /// Stop message so a test can fabricate a "dead" presence recipient.
    struct StopPresenceSub;

    impl Message for StopPresenceSub {
        type Result = ();
    }

    impl Handler<StopPresenceSub> for MockPresenceSub {
        type Result = ();
        fn handle(&mut self, _msg: StopPresenceSub, ctx: &mut Context<Self>) {
            ctx.stop();
        }
    }

    fn mock() -> (PresenceEvents, Recipient<PresenceChange>) {
        let events: PresenceEvents = Arc::new(Mutex::new(Vec::new()));
        let recipient = MockPresenceSub {
            events: events.clone(),
        }
        .start()
        .recipient();
        (events, recipient)
    }

    /// Poll a subscriber until it has recorded `len` events (or fail).
    async fn wait_events(events: &PresenceEvents, len: usize) -> Vec<(i64, bool)> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if events.lock().unwrap().len() >= len {
                return events.lock().unwrap().clone();
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {len} presence events; got {:?}",
                events.lock().unwrap().clone()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[actix::test]
    async fn presence_online_offline_broadcast() {
        let hub = PresenceHub::new().start();
        let (sub_events, recipient) = mock();
        hub.send(JoinPresence {
            alias_id: 1,
            conn_key: 1,
            recipient,
        })
        .await
        .unwrap();
        let got = wait_events(&sub_events, 1).await;
        assert_eq!(got, vec![(1, true)]);

        // A second player joining fans out to existing subscribers too.
        let (other_events, other) = mock();
        hub.send(JoinPresence {
            alias_id: 2,
            conn_key: 2,
            recipient: other,
        })
        .await
        .unwrap();
        let got = wait_events(&sub_events, 2).await;
        assert_eq!(got, vec![(1, true), (2, true)]);
        let got = wait_events(&other_events, 1).await;
        assert_eq!(got, vec![(2, true)]);

        // A mock that never registered receives nothing.
        let (bystander_events, _) = mock();
        assert!(bystander_events.lock().unwrap().is_empty());
    }

    #[actix::test]
    async fn presence_offline_only_on_last_disconnect() {
        let hub = PresenceHub::new().start();
        let (observer_events, observer) = mock();
        hub.send(JoinPresence {
            alias_id: 99,
            conn_key: 99,
            recipient: observer,
        })
        .await
        .unwrap();
        wait_events(&observer_events, 1).await;

        // Two connections under the same alias.
        let (e1, r1) = mock();
        let (e2, r2) = mock();
        hub.send(JoinPresence {
            alias_id: 42,
            conn_key: 100,
            recipient: r1,
        })
        .await
        .unwrap();
        wait_events(&e1, 1).await;
        hub.send(JoinPresence {
            alias_id: 42,
            conn_key: 101,
            recipient: r2,
        })
        .await
        .unwrap();
        wait_events(&e1, 1).await;

        // First disconnect: alias 42 still has a connection -> no offline.
        let before = observer_events.lock().unwrap().len();
        hub.send(LeavePresence {
            alias_id: 42,
            conn_key: 100,
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            observer_events.lock().unwrap().len(),
            before,
            "offline must not fire while a connection remains"
        );

        // Last disconnect -> surviving subscribers see the offline transition.
        hub.send(LeavePresence {
            alias_id: 42,
            conn_key: 101,
        })
        .await
        .unwrap();
        let got = wait_events(&observer_events, before + 1).await;
        assert_eq!(got.last(), Some(&(42, false)));

        // The departed recipients: the first saw the online announcement; the
        // second joined an already-online alias (not a transition) so it saw
        // nothing at all.
        assert_eq!(*e1.lock().unwrap(), vec![(42, true)]);
        assert!(e2.lock().unwrap().is_empty());
    }

    #[actix::test]
    async fn presence_unsubscribe_stops_delivery() {
        let hub = PresenceHub::new().start();
        let (e1, r1) = mock();
        hub.send(JoinPresence {
            alias_id: 1,
            conn_key: 10,
            recipient: r1,
        })
        .await
        .unwrap();
        wait_events(&e1, 1).await;

        hub.send(LeavePresence {
            alias_id: 1,
            conn_key: 10,
        })
        .await
        .unwrap();
        assert_eq!(
            *e1.lock().unwrap(),
            vec![(1, true)],
            "departed recipient must not receive its own offline broadcast"
        );

        // A later join fan-out must skip the departed recipient.
        let (other_events, other) = mock();
        hub.send(JoinPresence {
            alias_id: 2,
            conn_key: 20,
            recipient: other,
        })
        .await
        .unwrap();
        wait_events(&other_events, 1).await;
        assert_eq!(
            *e1.lock().unwrap(),
            vec![(1, true)],
            "unsubscribed recipient must receive nothing further"
        );
    }

    #[actix::test]
    async fn presence_join_reonline_after_cap_prune_of_dead_recipients() {
        let hub = PresenceHub::new().start();

        // Fill one alias to the cap with a single subscriber registered under
        // many conn keys, then stop it so every stale entry reports as
        // disconnected (simulating a missed LeavePresence for them all).
        let stale_events: PresenceEvents = Arc::new(Mutex::new(Vec::new()));
        let stale_addr = MockPresenceSub {
            events: stale_events.clone(),
        }
        .start();
        let stale_recipient: Recipient<PresenceChange> = stale_addr.clone().recipient();
        for key in 0..MAX_CONNS_PER_ALIAS {
            hub.send(JoinPresence {
                alias_id: 1,
                conn_key: key,
                recipient: stale_recipient.clone(),
            })
            .await
            .unwrap();
        }
        wait_events(&stale_events, 1).await;

        stale_addr.send(StopPresenceSub).await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while stale_recipient.connected() {
            assert!(
                Instant::now() < deadline,
                "stale presence subscriber did not stop"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // A fresh live join must prune the dead set and re-announce online.
        let (live_events, live_recipient) = mock();
        hub.send(JoinPresence {
            alias_id: 1,
            conn_key: MAX_CONNS_PER_ALIAS,
            recipient: live_recipient,
        })
        .await
        .unwrap();
        let got = wait_events(&live_events, 1).await;
        assert_eq!(got, vec![(1, true)]);
    }

    #[actix::test]
    async fn presence_leave_unknown_alias_ignored() {
        let hub = PresenceHub::new().start();
        hub.send(LeavePresence {
            alias_id: 7,
            conn_key: 1,
        })
        .await
        .unwrap();
    }

    #[actix::test]
    async fn presence_payload_shape() {
        let payload = presence_payload(42, true);
        assert_eq!(payload["res"], "v1.players.presence.updated");
        assert_eq!(payload["data"]["presence"]["online"].as_bool(), Some(true));
        assert_eq!(
            payload["data"]["presence"]["playerAlias"]["id"].as_i64(),
            Some(42)
        );
        assert!(payload["data"]["meta"]["onlineChanged"].is_boolean());
        assert!(payload["data"]["presence"]["lastSeenAt"].is_string());

        let offline = presence_payload(7, false);
        assert_eq!(offline["data"]["presence"]["online"].as_bool(), Some(false));
        assert_eq!(
            offline["data"]["presence"]["playerAlias"]["id"].as_i64(),
            Some(7)
        );
    }
}
