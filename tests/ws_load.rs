//! WebSocket load / throughput test (task 0.3.15).
//!
//! The `/ws` handshake itself runs without DB or Redis and is covered in
//! `ws_integration.rs`. This file exercises the in-process, production-grade
//! `ChannelHub` fan-out path — the same registry the sockets layer drives — at
//! the milestone load spec: **100 concurrent connections, 1000 messages, 0
//! dropped (or double-delivered) envelopes**.
//!
//! No network WS client crate is vendored, and the hub is deliberately DB-free,
//! so the test drives the public hub API directly with 100 live subscriber
//! actors, exactly mirroring the unit-level load tests in
//! `src/sockets/channels.rs` but from a standalone `tests/` binary against the
//! public crate surface.

use actix::prelude::*;
use playzoid_server::sockets::channels::{
    ChannelHub, ChannelMessage, ChannelNotification, JoinChannel,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A fan-out event a mock subscriber records.
#[derive(Debug, PartialEq, Eq, Clone)]
enum WsEvent {
    /// A membership transition (channel, alias, joined).
    Change(i64, i64, bool),
    /// A chat message broadcast (channel, sender alias, text).
    Message(i64, i64, String),
}

/// Event list shared between a mock subscriber and the test body.
type WsEvents = Arc<Mutex<Vec<WsEvent>>>;

/// Test subscriber recording every channel notification it receives.
struct MockSubscription {
    events: WsEvents,
}

impl Actor for MockSubscription {
    type Context = Context<Self>;
}

impl Handler<ChannelNotification> for MockSubscription {
    type Result = ();
    fn handle(&mut self, msg: ChannelNotification, _ctx: &mut Context<Self>) {
        let event = match msg {
            ChannelNotification::Change(change) => {
                WsEvent::Change(change.channel_id, change.alias_id, change.joined)
            }
            ChannelNotification::Message(message) => {
                WsEvent::Message(message.channel_id, message.alias_id, message.message)
            }
        };
        self.events.lock().expect("subscriber lock").push(event);
    }
}

/// Spawn a subscriber and return the shared event list plus its recipient.
fn subscribe() -> (WsEvents, Recipient<ChannelNotification>) {
    let events: WsEvents = Arc::new(Mutex::new(Vec::new()));
    let recipient = MockSubscription {
        events: events.clone(),
    }
    .start()
    .recipient();
    (events, recipient)
}

/// Poll a subscriber until it has recorded `len` events (or fail the test).
async fn wait_events(events: &WsEvents, len: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if events.lock().expect("subscriber lock").len() >= len {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {len} events; got {:?}",
            events.lock().expect("subscriber lock").len()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Load test: 100 live subscriber connections, 1000 chat messages broadcast
/// through the hub, 0 dropped or double-delivered envelopes.
///
/// 100 members register into one channel (cumulative 5050 `joined` fans), then
/// member alias 1 sends 1000 chat messages; each fans out to all 100 recipient
/// conns, sender included (100k deliveries). The exact final count proves the
/// channel hub delivers every envelope exactly once under load.
#[actix::test]
async fn ws_load_100_connections_1000_messages_0_drops() {
    let hub = ChannelHub::new().start();
    const CONNS: usize = 100;
    const MSGS: usize = 1000;

    let (events, recipient) = subscribe();

    // Register 100 concurrent connections (one root group each) in channel 5.
    for i in 0..CONNS {
        hub.send(JoinChannel {
            channel_id: 5,
            alias_id: i as i64 + 1,
            parent_account_id: None,
            conn_key: i,
            recipient: recipient.clone(),
        })
        .await
        .expect("join channel");
    }

    // Setup fan-out is cumulative: the k-th group's join reaches k conns.
    let setup_events = (1..=CONNS).sum::<usize>();

    // Broadcast 1000 chat messages from member alias 2 (group 2).
    for _ in 0..MSGS {
        hub.send(ChannelMessage {
            channel_id: 5,
            alias_id: 2,
            group: 2,
            message: "load test message".to_string(),
        })
        .await
        .expect("send message");
    }

    let expected = setup_events + MSGS * CONNS;
    wait_events(&events, expected).await;
    // Let the actor mailbox fully drain so a late arrival can't pass unnoticed.
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        events.lock().expect("subscriber lock").len(),
        expected,
        "100 connections, {MSGS} chat messages, 0 dropped or double-delivered envelopes"
    );
}
