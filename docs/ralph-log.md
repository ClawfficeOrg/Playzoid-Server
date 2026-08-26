# Ralph Log — Playzoid-Server

Timestamped entries appended by the ralph autonomous task agent.
Newest entries are at the bottom (append-only).

Format:
```
## YYYY-MM-DD HH:MM

<DONE|BLOCKED|PHASE_COMPLETE|...>: <task-id> — <brief description>
[Optional detail]
```

---

<!-- ralph appends entries below this line -->

## 2026-08-25 23:40

DONE: task 0.3.6 — implemented `GET /saves/{player_id}` (list saves).
Entity (`src/entities/save.rs`), service (`src/services/saves.rs`),
handler (`src/api/saves.rs`), wire-up (mod files + `main.rs`), unit tests
(3 API + entity + service), integration suite `tests/saves_integration.rs`
(5 `#[ignore]`d tests, Docker dev stack). Gate checks
(`fmt --check` / `clippy -D warnings` / `test`) all pass. Commit/push/PR
deferred by operator instruction — todo-v0.md status row left open pending PR.

## 2026-05-27 09:53

Session complete. Tasks completed: 0 of 0 attempted.

## 2026-05-27 11:13

BLOCKED on task 0.3-1: checks still failing after 3 attempts. Fix manually then re-run ralph.

## 2026-05-27 11:13

Session complete. Tasks completed: 0 of 1 attempted.

## 2026-08-25 21:01

DONE: 0.3.6 merged into release/v0.3.

## 2026-08-25 21:11

DONE: 0.3.7 merged into release/v0.3.

## 2026-08-25 23:55

DONE: task 0.3.8 — implemented `GET /saves/{player_id}/{save_id}` (retrieve save).
Service `get_save` (`src/services/saves.rs`), handler + route
(`src/api/saves.rs`), 3 API-layer unit tests, 6 `#[ignore]`d integration
tests in `tests/saves_integration.rs` (Docker dev stack). Own-only:
cross-player 403 before SQL; player-scoped SELECT so another player's save
or an unknown save id → 404 (never leaks). Gate checks
(`fmt --check` / `clippy -D warnings` / `test`) all pass. Commit/push/PR
deferred by operator instruction — todo-v0.md flipped to `- [x]` with a
done note pending PR.

## 2026-08-25 21:18

DONE: 0.3.8 merged into release/v0.3.

## 2026-08-25

DONE: task 0.3.9 — implemented `DELETE /saves/{player_id}/{save_id}` (delete save).
Service `delete_save` (`src/services/saves.rs`), handler + route
(`src/api/saves.rs`), 3 API-layer unit tests, 6 `#[ignore]`d integration
tests in `tests/saves_integration.rs` (Docker dev stack). Own-only:
cross-player 403 before SQL; player-scoped DELETE (`WHERE public_id = ? AND
player_id = ?`) so another player's save or an unknown save id → 404 (0 rows,
never leaks). Success → 204 No Content. Gate checks
(`fmt --check` / `clippy -D warnings` / `test`) all pass. Commit/push/PR
deferred by operator instruction — todo-v0.md flipped to `- [x]` with a
done note pending PR.

## 2026-08-25 21:22

DONE: 0.3.9 merged into release/v0.3.

## 2026-08-25 21:29

DONE: 0.3.10 merged into release/v0.3.

## 2026-08-25 21:44

DONE: 0.3.11 merged into release/v0.3.

## 2026-08-25 21:55

IMPLEMENTED: 0.3.12 (WebSocket channel join/leave message types) on
task/0.3.12. New `src/sockets/channels.rs` — in-memory `ChannelHub` actor
(`JoinChannel`/`LeaveChannel`/`LeaveAllChannels`/`ChannelChange`), verified Talo
`v1.channels.player-joined`/`player-left` payload builders (`meta.reason` numeric
0), reverse conn→memberships index, cap+prune mirroring `presence.rs`. Wired in
`ws.rs`: `process_text_frame` → pure `FrameOutcome` enum (`None`/`Identify`/
`JoinChannel`/`LeaveChannel`), `v1.channels.join`/`leave` gated on identify +
integer `channelId`, dispatched to the hub with the ticketed alias (never
client-supplied); `stopping()` also sends `LeaveAllChannels`; `WsConn` renders
`ChannelChange`. `mod.rs` registers the module. **Documented deviation:** the
join/leave *request* tokens are a Playzoid socket extension — upstream Talo has
no such request (membership is HTTP-driven); response envelopes stay 100%
Talo-verified (decision recorded in `docs/memory.md`, surfaced here per the
do-not-guess rule). Gate checks (`fmt --check` / `clippy -D warnings` /
`test`) pass. Commit/push/PR deferred by operator instruction.

## 2026-08-25 22:04

DONE: 0.3.12 merged into release/v0.3.

## 2026-08-25 22:41

IMPLEMENTED: 0.3.13 (WebSocket chat message broadcast within channel) on
task/0.3.13. `ChannelHub` now fans a verified-Talo `v1.channels.message`
envelope (`{ res, data: { channel: { id }, message: <string>, playerAlias: { id } } }`)
to every member socket, sender included, via a joint `ChannelNotification`
enum (`Change | Message`) stored once per connection — a single registry
carries both the 0.3.12 join/leave and chat fan-outs, no duplicate
bookkeeping. Upstream shape re-verified against
`TaloDev/backend/src/socket/listeners/gameChannelListeners.ts` (fan-out to all
members incl. sender; message is a plain string; non-member send rejected
upstream). ws layer: `v1.channels.message` becomes an identify-gated `&self`
method validating integer `channelId` + non-empty message ≤
`MAX_CHAT_MESSAGE_CHARS` (1000), sender alias stamped from the server-resolved socket
ticket only (spoofed `playerAliasId` claims ignored); `FrameOutcome` gains
`BroadcastMessage`; `WsConn` implements `Handler<ChannelNotification>`;
`MSG_SEQ`/echo message-id shape dropped. **Documented deviations:** (1) the
request field is `channelId` (Playzoid socket extension like 0.3.12 join/leave;
upstream nests `channel.id`), response envelope stays Talo-verified; (2)
non-member send or unknown/empty channel is a silent no-op instead of Talo's
"Player not in channel" rejection — no sender-reachable error channel in v0
(both recorded in `docs/memory.md`, surfaced here per the do-not-guess rule).
Load test: 100 conns × 1000 chat broadcasts, 0 dropped/double-delivered.
Gate checks (`fmt --check` / `clippy -D warnings` / `test`) pass. Commit/push/PR
deferred by operator instruction — todo-v0.md flipped to `- [x]` with a done
note pending PR.
