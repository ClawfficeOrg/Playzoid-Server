# Playzoid-Server — Learnings

Per-task learnings: what worked, what bit, what to do differently.
One `## X.Y.Z — Title` section per task, newest first. Keep entries short —
bullets over prose. Read the last few sections before starting a similar task.

## 0.3.14 — WS subaccount participant grouping

- Group keys derive server-side from the ticketed alias only
  (`SELECT parent_account_id … WHERE status <> 'deleted'`); never accept a
  group/parent from the client. Grouping is a DB lookup on connect, best-effort:
  missing pool / unknown alias / lookup error → per-alias identity, and the
  connection still proceeds.
- Rekey membership to `channel → group → alias → conn_key` and widen the
  reverse index to `conn_key → (channel, group, alias)`. The alias must live in
  the reverse index, otherwise the group's last-conn `player-left` cannot name
  *which* alias departed. Learning a new dimension of the same hub pattern.
- Group-level leaf-ness ≠ alias-level: a group is a participant until its *last
  conn across all its aliases* leaves. Multi-alias groups (parent + subaccount)
  need the same refcount discipline one level up.
- `Option<web::Data<MySqlPool>>` is the actix idiom for a DB pool that may be
  absent (degraded mode); `web::Data<T>` alone makes the handler 500. Existing
  no-pool integration tests double as the extractor's regression guard.
- Identifier widening: `players.id` is BIGINT UNSIGNED — decode `u64` (matches
  `entities::Player`), widen to `i64` at the socket layer, never bind an i64
  into an unsigned column where types could mismatch.
- Keep broadcast envelopes Talo-verified: surface the parent relationship as an
  *additive* optional field in a request-side response (`identify.success`),
  never as a change to the verified fan-out shapes.

## 0.3.12 — WS channel join/leave

- Upstream Talo has no `v1.channels.join`/`leave` request token — membership is
  HTTP-driven and sockets only get the `player-joined`/`player-left` fan-out.
  Mirroring the response side exactly and adding socket requests as a
  documented extension beats inventing an envelope shape. Keep the deviation
  visible in memory.md + PR body for the reviewer.
- A `join` with multiple connections per alias is a pure registry add — no
  broadcast. Only the alias's *first* conn (join) and *last* conn (leave) are
  transitions. Cost the same refcount discipline as presence.
- Keep a reverse `conn_key → memberships` index or `LeaveAllChannels` on
  disconnect is O(all channels); with it, it's O(that conn's channels).
- Fan-out ordering matters: remove the departed conn's recipient *before*
  broadcasting `player-left`, or the leaver receives its own obituary.
- `meta.reason` is a TS numeric enum — serialize as JSON number, not string.

## 0.3.11 — WS presence broadcast

- actix `Recipient` mailboxes give no ack — test hub broadcasts by polling an
  `Arc<Mutex<Vec>>` that a mock actor's `Handler` appends to, with a deadline
  loop. Hub `send().await` only orders the *hub's own* handler, not the
  spawned recipients.
- Spawn process-global actors from inside the WS request context via
  `once_cell::Lazy<Addr<…>>::new(|| Actor::new().start())` — the arbiter that
  first touches the `Lazy` lives for the system lifetime, so no `main.rs`
  wiring is needed for a single-process hub.
- Keep side effects out of the actor plumbing: `process_text_frame(&mut self)
  -> (Vec<Value>, bool)` + `leave_presence() -> Option<…>` mean the
  join/leave decisions are unit-testable without a live websocket.
- A second connection to an already-online alias is **not** a transition — no
  broadcast fires, not even to the new socket. Test that, don't assert a dup
  online event.

## 0.3.4 — PUT leaderboard entry

- MySQL returns `COUNT(*)` as signed BIGINT — decode into `i64`, not `u64`,
  or sqlx raises `ColumnDecode`. Same applies to any aggregate on the wire.
- Order HTTP-layer guards cheapest-first so status codes win predictably:
  body validation (400) → ownership (403) → pool availability (503) → DB.
- Integration tests that probe rows with `WHERE score = N` collide across
  tests sharing the dev DB — always scope by the test's unique board/player.

## 0.3.3 — POST leaderboard entry

- Enable sqlx's `json` feature before binding `serde_json::Value` to a JSON
  column — compile passes without it only if you never bind the type.
- One-entry-per-player semantics come free from the unique
  `(leaderboard_id, player_id)` constraint; map the dup error to 409 and
  point clients at PUT instead of upserting silently.
- Rank query pattern: `1 + COUNT(higher scores OR equal score with lower id)`
  keeps ranks stable across ties and pagination.

## 0.3.1/0.3.2 — Leaderboard schema + GET endpoint

- Stacked task branches (`task/A` → `task/B`) avoid conflicts when later
  tasks depend on earlier unmerged ones — but expect CHANGES.md/TODO.md merge
  conflicts on every merge-to-main; resolve top-down.
- Put ranking indexes `(leaderboard_id, score DESC)` in the initial migration,
  not as an afterthought — paginated top-score queries lean on them.

## Earlier phases

- Integration tests default to the Docker dev stack (`#[ignore]`d); run with
  `cargo test --test '*' -- --ignored` while compose is up.
- Unique usernames (`u<uuid12>`) prevent cross-run collisions on the shared
  dev database.

## 0.4.8 — Redis rate limiting

- Cloning a `ServiceRequest` for inspection then building a response from the
  clone panics: the inner `HttpRequest` lives in an `Rc`, and actix's
  `match_info_mut()` requires refcount 1. Decide the path first, then consume
  the request once (`req.into_response(...)` for 429, `service.call(req)`
  otherwise) — never clone.
- A middleware whose `call` returns `Pin<Box<dyn Future + 'static>>` cannot
  capture `&self`. Don't reach for `S: Clone`; store the service as `Arc<S>`
  (needs only `S: 'static`) and clone the `Arc` before the async block.
- `web::Data<T>` downcasts on the concrete type, generics included. A generic
  `RateLimiter<C>` in `app_data` silently mismatches the middleware's lookup —
  the middleware just never runs. Erase to `Arc<dyn WindowCounter>` (or
  similar) so the stored type is monomorphic.
- Object-safe async traits need `Pin<Box<dyn Future + Send>>` returns; to keep
  them `'static`, copy owned captures (connection clone, key string) before
  building the future.
