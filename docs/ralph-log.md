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

## 2026-08-25 22:20

DONE: 0.3.13 merged into release/v0.3.

## 2026-08-25 22:35

DONE: task 0.3.14 — WebSocket subaccount participant support
(parent_account_id grouping), implemented on task/0.3.14.
- `src/sockets/groups.rs` (new): `resolve_parent_account_id` (parameterized
  `SELECT parent_account_id … WHERE status <> 'deleted'`), pure `group_key`
  (root → self, subaccount → parent), `resolve_group`; `#[ignore]`d live-DB
  test mirroring the `db.rs` R-7 `MYSQL_URL` pattern.
- `src/sockets/channels.rs`: registry rekeyed to
  `channel → group → alias → conn_key → Recipient<ChannelNotification>` with
  reverse `conn_key → (channel, group, alias)`; join/leave/broadcast/prune/
  leave-all at group level. `JoinChannel`/`LeaveChannel` carry the resolved
  `parent_account_id`; `ChannelMessage` carries the server-stamped `group`
  (send gate = group-level membership); `LeaveAllChannels` keys on `conn_key`
  only. Envelopes stay Talo-verified per-alias (grouping visible only in
  membership sharing).
- `src/sockets/ws.rs`: `WsConn.parent_account_id`; `ws_index` gains an
  `Option<web::Data<MySqlPool>>` extractor, best-effort resolution on connect
  (pool absent / unknown alias / lookup error → None, never fails the
  connection); join outcome carries the parent; additive `parentAccountId` in
  `v1.players.identify.success` data.
- Tests: hub group-semantics suite (join-once-per-group, parent↔sub chat
  sharing, distinct parents distinct participants, group leave on last conn,
  group-level send gate), `group_key` pure tests, ws.rs subaccount stamping +
  join-parent tests, updated load tests (100 conns × 1000 broadcasts / 1000
  chat, 0 drops, mixed root/subaccount groups), `tests/ws_integration.rs`
  no-pool upgrade regression guard (existing no-pool tests also cover the
  Option<Data<Pool>> absent path).
- **Documented deviations/decisions:** (1) grouping is server-resolved from the
  ticketed alias only — spoof-proof; (2) additive `parentAccountId` in
  `identify.success` instead of touching verified broadcast envelopes (Talo
  parity doctrine from 0.3.12/0.3.13); (3) degraded-DB mode = per-alias
  identity; (4) one-hop immediate parent only (nested-subaccount roots are
  follow-up) — all recorded in `docs/memory.md`; surfaced here per the R-7/M
  do-not-guess rule.
Gate checks (`fmt --check` / `clippy -D warnings` / `test`) pass. Commit/push/
PR deferred by operator instruction — todo-v0.md flipped to `- [x]` with a
done note pending PR.

## 2026-08-25 22:38

DONE: 0.3.14 merged into release/v0.3.

## 2026-08-25 22:39

Time limit reached. Completed: 9.

## 2026-08-25 23:07

DONE: 0.3.15 merged into release/v0.3.

## 2026-08-25 23:15

DONE: 0.3.16 — leaderboard + save unit/integration test gap-fill. 10 new
tests/leaderboards_integration.rs, 5 new tests/saves_integration.rs,
entity unit tests in src/entities/leaderboard.rs (mirrors save.rs — owned-path
justification in commit). fmt/clippy/test green; full ignored integration
suite run vs Docker stack: 26/26 leaderboards, 29/29 saves.

Pending: commit + PR (Closes task 0.3.16) + release/v0.3 merge.

## 2026-08-25 23:15

DONE: 0.3.16 merged into release/v0.3.

## 2026-08-25 23:22

DONE: 0.3.17 merged into release/v0.3.

## 2026-08-25 23:22

PHASE_BLOCKED: 0.3 review failed after 3 attempts. See /tmp/ralph-line-review-0.3-3.log.

## 2026-08-25 23:4x

PHASE_COMPLETE: 0.3 merged to main; tagged v0.3.0. Release review performed manually (checklist 6/6 PASS) after RELEASE_REVIEW_AGENT was blocked by Copilot org policy.

## 2026-08-26 — task/0.4.1 run log

- Implemented `/v1` route-prefix parity: `auth`, `players`, `leaderboards`,
  `saves` now mount canonical `/v1/<group>` scopes plus legacy unprefixed
  aliases via a shared per-module `scoped(prefix)` builder (single route
  definition, no drift). `main.rs` untouched (config signatures unchanged).
- Unit tests: repointed to `/v1/*`, added legacy-alias routing tests per module.
- Integration tests: all flows repointed to `/v1/*`; added one legacy-path
  test per suite (auth register+login, players get, leaderboards submit-then-
  read-across-mounts, saves create/list/delete roundtrip).
- Doc-comment-only path refreshes in `src/services/auth.rs` and
  `src/services/players.rs` (no other active-task owner; comment-only).
- Known staleness: `docs/TALO_API.md` still describes pre-parity prefixes and
  the Phase 0.2 decision note at its end. NOT edited here — file is owned by
  tasks 0.4.2 / 0.4.13; recorded instead of edited to avoid cross-task edits.

## 2026-08-26 05:50

DONE: 0.4.1 merged into release/v0.4.

## 2026-08-26 — task 0.4.2 (branch task/0.4.2)

DONE (implementation; no commit/PR per session instructions):
- `src/entities/`: new `prop.rs`, `player_auth.rs`, `player_alias.rs`,
  `game_channel.rs`; `leaderboard.rs` gains `LeaderboardSortMode` + full
  upstream-parity `LeaderboardEntry` beside the untouched view structs;
  modules registered in `mod.rs`.
- Shapes re-verified live against docs.trytalo.com (sockets/responses,
  leaderboard-api, game-channel-api). Two plan refinements recorded in
  docs/memory.md: `GameChannel.owner` = full `Option<PlayerAlias>` (no
  recursion, so the provisional AliasSummary is unnecessary), and alias
  timestamps `Option` (leaderboard samples omit them).
- 20 new entity unit tests; security invariant (no password material in
  PlayerAuth/PlayerAlias) pinned by tests.
- Docs: TALO_API.md "Domain models (upstream parity)" section + ticked its
  Remaining-TODOs bullet only (endpoint sections stay owned by 0.4.13);
  CHANGES.md + memory.md entries.
- Gate: cargo fmt --check / clippy -D warnings / cargo test all green
  (163 lib tests passed).
