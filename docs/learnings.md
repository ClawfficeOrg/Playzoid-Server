# Playzoid-Server — Learnings

Per-task learnings: what worked, what bit, what to do differently.
One `## X.Y.Z — Title` section per task, newest first. Keep entries short —
bullets over prose. Read the last few sections before starting a similar task.

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
