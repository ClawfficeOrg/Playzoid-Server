# Playzoid-Server — Memory Index

Master memory file. High-level architectural decisions that apply across
milestones live here; per-milestone detail lives in the versioned files below.
Do not rewrite history — supersede entries with new dated ones.

## Versioned Memory Files

| Milestone | File | Status |
|-----------|------|--------|
| v0 — Foundation, Auth, Leaderboards | (this file) | 🔄 Active |

## Hard Rules

1. **Never delete files** without explicit instruction. `mv` to `/tmp/`
   (recycle) instead of `rm`.
2. **Never push directly to `main`** — all work goes through PRs
   (see `AGENTS.md`).
3. **No secrets in code or docs** — credentials live in `.env` / env vars.

## Architectural Decisions (Cross-Cutting)

### Argon2id for password hashing
Established 2026-04-30. All `password_hash` columns store PHC-format
Argon2id strings; bcrypt is not a dependency. Tuning parameters live in
`Config`. Full rationale: dated entry below.

### Docker dev stack is the live integration test target
Established 2026-05-27. Integration tests are `#[ignore]`d by default and run
against `docker compose -f config/docker-compose.dev.yml` (MySQL + Redis).
Override with `DATABASE_URL` / `REDIS_URL`.

### MySQL is canonical; PostgreSQL out of scope until 1.0.0
sqlx uses the `mysql` feature only. Migrations are MySQL-flavoured SQL.

---

# Decision Log

> Long-term decision record for the Playzoid-Server crate. New decisions are
> appended at the bottom with a date stamp, the alternatives considered, and
> the reasoning that won.

This file was previously `memory/projects/playzoid-server/project.md`; it moved
to `docs/memory.md` as part of the standard repo layout.

---

## Decisions

### 2026-04-30 — Use Argon2id (not bcrypt) for password hashing

- **Context:** Phase 0.2 introduces real authentication (`POST /auth/login`,
  `POST /auth/register`). The original `docs/PLAN.md` mentioned both bcrypt
  and Argon2 as candidates without picking one.
- **Options considered:**
  - `bcrypt` — battle-tested, simple, but capped at 72-byte passwords and
    older against modern GPU/ASIC attacks.
  - `argon2` (Argon2id variant) — winner of the 2015 Password Hashing
    Competition, memory-hard, current OWASP recommendation.
  - `scrypt` — also memory-hard but with less ecosystem momentum than
    Argon2id today.
- **Decision:** Use **Argon2id** via the `argon2` crate (already pinned in
  `Cargo.toml`). All `password_hash` columns store the PHC-format
  `$argon2id$...` string; verification uses `argon2::PasswordVerifier`.
- **Consequences:**
  - The `players.password_hash` column is sized `VARCHAR(255)` to fit the
    PHC encoding plus salt + hash.
  - Tuning parameters (`m_cost`, `t_cost`, `p_cost`) live in `Config` so we
    can ratchet them without a migration.
  - Bcrypt is **not** added as a dependency; legacy hashes are not in scope.

---

### 2026-05-27 — Docker dev stack is the live integration test target

- **Context:** Integration tests needed a live MySQL + Redis. The project already
  had a Docker Compose dev stack confirmed healthy from the Phase 0.2 smoke
  test session.
- **Decision:** Integration tests default to
  `mysql://playzoid:password@127.0.0.1:3306/playzoid_dev` and
  `redis://127.0.0.1:6379` (the Docker dev stack). Both URLs are overrideable
  via `DATABASE_URL` / `REDIS_URL` env vars for CI or alternate environments.
- **Pattern:** Tests are marked `#[ignore]` so `cargo test` skips them;
  `cargo test -- --ignored` runs the full suite when Docker is up.
- **Consequences:** No in-memory test DB; tests use UUID-prefixed unique
  usernames to avoid conflicts on repeated runs.

---

### 2026-08-25 — Game saves are own-only reads returning full blobs

- **Context:** Task 0.3.6 adds `GET /saves/{player_id}`. Profile reads
  (`GET /players/{id}`) are free reads for any authenticated user, but game
  saves are private per-player game state, and Talo's `game-saves.getAll`
  returns full save blobs.
- **Options considered:**
  - Free-read (any authenticated user may list anyone's saves) — matches
    profile reads but leaks private game state.
  - Own-only read with 403 for cross-player — treats saves like leaderboard
    entries; private by default.
  - Metadata-only list (no blobs) — smaller payloads but breaks parity with
    `game-saves.getAll` and forces an extra fetch per slot.
- **Decision:** Saves are **own-only** — a request whose `{player_id}`
  differs from the JWT identity returns 403 before any SQL. Lists return the
  full `SaveView` objects **including blobs**, newest first
  (`created_at DESC, updated_at DESC`). No pagination for this Small task; a
  player with no saves gets 200 `[]`. A parent may *not* read a subaccount's
  saves (open question pending product input).
- **Consequences:**
  - `/saves` gets no Redis cache (matches the leaderboard services; DB-only).
  - No internal `id` is selected in the list query — the ORDER BY uses
    `updated_at` explicitly as the tie-break, keeping `SaveRow` free of dead fields.
  - Single-save retrieval (0.3.8) and creation (0.3.7) reuse the same
    `SaveView` shape.

---

### 2026-08-25 — Save creation is own-only with an optional Talo-shaped `playerId`

- **Context:** Task 0.3.7 adds `POST /saves`. Talo's verified `CreateSaveRequest`
  carries a `playerId` field, but the 0.3.6 decision made saves private
  per-player (own-only reads).
- **Options considered:**
  - Trust a required `playerId` from the body — lets any caller create saves
    for arbitrary players.
  - Ignore `playerId` entirely — breaks Talo SDK client compatibility (they
    send it).
  - Optional `playerId` validated against the JWT — accept when it matches,
    403 when it differs, default to the JWT identity when absent.
- **Decision:** `playerId` stays optional and is validated against the JWT
  identity (403 on mismatch); `None` defaults to the caller, so creation can
  never target another player. Combined serialized `save` + `metadata` is
  capped at 32 KiB (`MAX_SAVE_BYTES`) — under InnoDB's 65,535-byte row limit
  and mirroring the `MAX_PROPS_BYTES` precedent. JSON `null` save blobs are
  rejected up-front so the NOT NULL column never surfaces a misleading 500.
  Create reuses the 0.3.6 `SaveView` projection; an insert `public_id` UUID
  collision retries once with a fresh UUID.
- **Consequences:**
  - The API layer raises body-validation 400s (unknown fields, empty/malformed
    name) before the ownership 403 check, which itself precedes the
    pool-unavailable 503; the service re-validates (trimmed name, null blob,
    size cap) before any SQL, so those 400s beat SQL-time failures (404
    player-not-found, 500) whenever a pool is present.
  - Delete (0.3.9) and single-save retrieval (0.3.8) follow the same own-only
    + `SaveView` conventions.

---

### 2026-08-25 — Single-save reads are scoped by internal player id (defense-in-depth)

- **Context:** Task 0.3.8 adds `GET /saves/{player_id}/{save_id}`. The API
  layer already rejects mismatched `{player_id}` with 403 pre-pool, but the
  service query could still have been `WHERE public_id = ?` alone.
- **Decision:** `get_save` resolves the owning player to its internal BIGINT
  id (active-only) and selects `WHERE public_id = ? AND player_id = <internal>`.
  A save belonging to another player — or an unknown save id — therefore
  surfaces as the same 404 as an unknown player, so clients cannot distinguish
  (and never see) another player's save ids. Same pattern applies to the
  0.3.9 delete.
- **Consequences:** No new error variant needed; `SaveServiceError::NotFound`
  already covers both unknown-player and unknown-save reads.

---

<!-- Append new decisions below this line. Use the dated heading format above. -->

## 2026-08-25 — WebSocket `/ws` gating and message envelope (task 0.3.10)

Established: socket auth is a one-shot ticket (`POST /v1/socket-tickets` →
`?ticket=` query param on the ws upgrade), matching the Talo socketToken
flow; actor stores the resolved `alias_id`. Missing/invalid tickets still
upgrade so the client gets the `v1.error` envelope (`INVALID_SOCKET_TOKEN`),
then the socket closes with policy code — no silent unauthenticated sessions.
Inbound envelope parsing was extracted to a pure `process_text_frame`
function so happy/error paths are unit-testable without a live socket; all
input failures return typed `v1.error` codes (`INVALID_JSON`,
`UNHANDLED_REQUEST`, `INVALID_INPUT`) instead of defaulting to 0/"".
Message ids are timestamp + atomic counter (unique within the same second).

---

## 2026-08-25 — Socket presence = single-process in-memory hub, register-on-identify (task 0.3.11)

**Context:** Task 0.3.11 adds the Talo `v1.players.presence.updated` broadcast
for player connect/disconnect. Upstream fires presence from the socket layer:
`player.setPresence(true)` when a player identifies, `setPresence(false)` when
their connection closes, envelope
`{ presence: { playerAlias, online, customStatus, lastSeenAt }, meta: { onlineChanged, customStatusChanged } }`
fanned out to all connected authed conns.

**Options considered:**
- Register on ticket-authenticated connect (earliest possible) — announces
  "online" for sockets that may never identify; deviates from Talo timing.
- Register on successful `v1.players.identify` (chosen, Talo parity) — a
  presence registration is armed only when the client actually claims its
  alias; the same boolean gates the `stopping()`-issued leave.
- Shared store / pub-sub (Redis) — multi-node correct but out of scope for v0
  and would force `main.rs` + Cargo wiring outside this task's owned path.

**Decision:** Single-process actix `PresenceHub` actor, process-global via a
`Lazy` accessor in `src/sockets/presence.rs`. Registry is
`alias → { conn_key → Recipient<PresenceChange> }`; `online` fires on the
alias's first identified connection, `offline` on the last disconnect, fanned
out to all registered recipients. `alias_id` in the registry always comes from
the server-resolved socket ticket — never a client-supplied id — so presence
cannot be spoofed. Connections get a unique `conn_key` (atomically assigned)
so a dropped socket is unregistered exactly once via `stopping()`.

**Consequences:**
- Single-node only: multi-instance deployments need a shared presence store
  (follow-up, tracked with the multi-node work). `customStatus` is accepted in
  the envelope shape but fixed to `null` / `customStatusChanged: false`
  (follow-up).
- Per-alias cap (256 conns) prunes dead recipients so a missed
  `LeavePresence` cannot grow the registry without bound.
- Tests exercise the hub directly (mock recipients) and the wire path through
  pure ws.rs helpers; no live stack required.

---

## 2026-08-25 — Socket channel join/leave: socket-driven extension over verified Talo response shapes (task 0.3.12)

**Context:** Task 0.3.12 adds the `v1.channels.player-joined` /
`v1.channels.player-left` broadcasts for WebSocket game-channel membership.
Upstream Talo (verified from `TaloDev/backend` + `TaloDev/docs`) fan-outs these
envelopes over sockets but has **no** `v1.channels.join` / `leave` request
token — membership is driven over HTTP, and sockets only receive the fan-out.
Playzoid v0 has no channel persistence or HTTP channel routes yet.

**Options considered:**
- Broker membership purely over the verified response side with no request
  trigger — nobody could ever join a channel, making the feature dead code.
- Add `v1.channels.join` / `v1.channels.leave` as documented Playzoid **request**
  extensions (chosen) — the only in-scope trigger; response envelopes stay
  Talo-verified, so clients that only consume broadcasts see identical shapes.
- Pull HTTP channel management into this task — scope creep into future
  tasks/owned paths.

**Decision:** `v1.channels.join` / `v1.channels.leave` are Playzoid socket
request extensions. Membership is tracked in a single-process in-memory
`ChannelHub` (structural mirror of `PresenceHub`): `channel → alias → conn_key
→ Recipient<ChannelChange>`, plus a reverse `conn_key → memberships` index so a
disconnect leaves all its channels in O(its channels). Alias ids always come
from the server-resolved socket ticket (never client-supplied). Joining an
already-member alias is idempotent (SDK-doc "already in channel → nothing
happens"). The joiner's own connection is included in the `player-joined`
fan-out (mirrors presence-hub insert-then-broadcast); the departed connection is
excluded from its own `player-left`. `meta.reason` is emitted as the numeric
`GameChannelLeavingReason::DEFAULT` (`0`), per the upstream TS numeric enum —
never a string. No `TEMPORARY_MEMBERSHIP` variant yet (belongs to subaccount /
temp-membership work).

**Consequences:**
- Single-node only, same as presence; multi-instance needs a shared store
      (tracked with the multi-node work).
    - `v1.channels.leave` is a socket-driven explicit leave; disconnect auto-leaves
      all channels via `stopping()` → `LeaveAllChannels`.
    - This is a **documented deviation** from upstream (request side only); if a
      reviewer rejects it the task is BLOCKED pending a human decision. Response
      envelopes remain Talo-verified.
    - Chat broadcast (task 0.3.13) will route through this membership registry.

---

## 2026-08-25 — Socket chat broadcast: joint-notification registry + Talo-parity envelope (task 0.3.13)

**Context:** Task 0.3.13 adds the `v1.channels.message` fan-out. Upstream Talo
(verified from
`TaloDev/backend/src/socket/listeners/gameChannelListeners.ts`) accepts
`{ channel: { id }, message: string }` and fans
`{ res: "v1.channels.message", data: { channel, message: <string>, playerAlias } }`
out to **every member socket, sender included**; a sender who is not a member
is rejected ("Player not in channel").

**Options considered:**
- Keep the 0.3.10-era echo shape (`data.message = { id, from: "server", message }`)
  and just broadcast that — preserves the earlier doc but diverges from the
  verified Talo fan-out (plain-string message + sender `playerAlias`).
- Register a second recipient per connection for chat alone — duplicates
  bookkeeping and doubles the fan-out registry.
- Re-type the single registry to a joint notification enum (chosen) — one
  `Recipient<ChannelNotification>` (`Change | Message`) per connection carries
  both membership changes and chat messages through the same fan-out paths.
- Reject non-member sends with Talo's "Player not in channel" envelope — needs
  a sender-reachable error channel that v0 sockets lack; instead a non-member
  send is a silent no-op (mirrors the existing non-member leave no-op).

**Decision:** `v1.channels.message` is a Playzoid **request** extension
(`channelId` field, same as the 0.3.12 join/leave extension; upstream nests
`channel.id`), while the response envelope stays Talo-verified. The channel
hub's membership registry stores `Recipient<ChannelNotification>` once per
connection; join/leave and chat messages fan out through that single registry.
The broadcast envelope is
`{ res, data: { channel: { id }, message: <string>, playerAlias: { id } } }` —
message is a plain string, the sender identity is always the server-resolved
socket-ticket alias (never a client-supplied `playerAliasId`), and the sender's
own connection receives its message (Talo parity). Inbound messages are
validated at the ws layer: identify-gated, integer `channelId`, non-empty
`message`, capped at `MAX_CHAT_MESSAGE_CHARS` (1000 chars). Sending as a
non-member, or into an unknown/empty channel, is a silent no-op for v0.

**Consequences:**
- The 0.3.10-documented counterpart-gated echo (`{ id, from: "server", message }`,
  timestamp+counter message ids) is dropped; envelopes changed. Recorded in
  `CHANGES.md`.
- Non-member sends produce no error to the sender (v0 trade-off); revisit when
  a socket error channel exists.
- Single-node only, same as presence / 0.3.12; multi-instance chat needs a
  shared store (tracked with the multi-node work).

---

## 2026-08-25 — Socket channel participation is group-keyed by parent_account_id (task 0.3.14)

**Context:** Task 0.3.14 adds subaccount participant support to WebSocket game
channels. `TaloRustServerPlan.md` docifies a "Subaccount Chat Extension" where
subaccounts appear as distinct users but share channel membership with their
parent. The verified upstream envelope set (`v1.channels.*`,
`v1.players.presence.updated`) carries per-alias `playerAlias` ids and never a
parent relationship, so the grouping must live server-side.

**Options considered:**
- Group-keyed participation at the registry level (chosen): a channel's
  participant set is its distinct groups with ≥1 live conn, where
  `group(alias) = players.parent_account_id` (server-resolved) or the alias
  itself for root accounts. First conn of a group announces `player-joined`
  (carrying the joining alias); last conn announces `player-left` (carrying the
  departing alias); chat fans to every conn across the channel's participant
  groups. Parent and subaccount conns therefore share membership and each
  other's chat.
- Parent id as an envelope field on broadcasts — breaks Talo parity; rejected
  (parity doctrine from 0.3.12/0.3.13). An **optional, additive**
  `parentAccountId` is instead surfaced in the `v1.players.identify.success`
  data (null for roots / degraded DB).
- Per-alias membership with parent-implied reads — requires walking parent
  trees per broadcast; no in-scope trigger and more state.

**Decision:** The single-process `ChannelHub` registry is rekeyed to
`channel → group → alias → conn_key → Recipient<ChannelNotification>` with a
reverse `conn_key → (channel, group, alias)` index (the alias is kept so a
group's last-conn `player-left` announces the departing alias). The group is
always derived server-side from the ticketed alias via a parameterized
`SELECT parent_account_id FROM players WHERE id = ? AND status <> 'deleted'`;
`resolve_parent_account_id` + pure `group_key` live in the new
`src/sockets/groups.rs`. Immediate parent only (one hop, mirrors the one-level
parent resolution used elsewhere); nested-subaccount roots are follow-up.
Envelopes keep the Talo-verified per-alias shape — grouping is visible only in
who shares a channel's membership. `v1.channels.join` / `leave` /
`message` remain Playzoid request extensions (0.3.12/0.3.13). Presence stays
per-alias, so subaccounts still appear as distinct users there.
**Degraded mode:** when the DB pool is absent, the alias is unknown, or the
lookup fails, `parent_account_id = None` and every alias groups as itself —
today's behavior; group resolution never fails a connection.

**Consequences:**
- `JoinChannel`/`LeaveChannel` carry the resolved `parent_account_id`; the hub
  computes the group key. `ChannelMessage` carries the sender's server-stamped
  `group`; the send gate is group-level membership (`group` must be a channel
  participant), so a subaccount of a participant parent may send.
- Spoof-proofing preserved: group always derives from the ticketed alias →
  `players.id`, never a client-supplied value (additive `parentAccountId` in
  `identify.success` is server data, not client input).
- A pre-existing v0 gap noted, not fixed (outside `src/sockets/` owned path):
  `POST /v1/socket-tickets` accepts any client-supplied alias id. A lookup miss
  just disables grouping for that alias — no privilege path, because the group
  remains server-derived.
- Single-node only, same as presence/chat; multi-instance needs a shared store
  (tracked with the multi-node work).

---

## 2026-08-26 — `/v1` route-prefix parity via dual-mount aliases (task 0.4.1)

**Decision:** the four gameplay route groups (`auth`, `players`, `leaderboards`,
`saves`) are mounted at both the canonical upstream-prefixed path
(`/v1/<group>`, matching Talo) and the legacy unprefixed path (`/<group>`)
during the transition. Each module builds its routes once in a private
`scoped(prefix)` helper and `config` registers the scope twice, so the two
mounts cannot drift; the public `config(&mut web::ServiceConfig)` signatures
are unchanged and `main.rs` is untouched.

**Rationale:** upstream parity (task 0.4.1) without breaking existing clients
in one step. The legacy alias removal is deliberately deferred to a later
hardening task — when it happens, only the second `.service(scoped(...))`
call per module plus the alias tests need deleting.

**Deliberately unprefixed (infra, not part of the upstream API surface):**
`/healthz` and `/ws`. `/v1/socket-tickets` already carried the prefix.

**Consequences:**
- The routed surface temporarily includes both spellings of every gameplay
  endpoint — documented intent for the transition window.
- Legacy-alias coverage lives in dedicated tests (unit + integration) so the
  eventual removal is a visible, test-driven change.
- `docs/TALO_API.md` still describes the pre-parity prefixes; its update is
  owned by task 0.4.13 (noted in `docs/ralph-log.md`).

---

## 2026-08-26 — Upstream domain-model struct lift (task 0.4.2)

**Context:** task 0.4.2 asks for full serde structs for `PlayerAlias`,
`PlayerAuth`, `GameChannel`, and the complete `LeaderboardEntry` (incl.
upstream `props`). `docs/TALO_API_STRUCTS.md` only catalogues request bodies,
so the domain shapes were re-verified against the live upstream docs:
docs.trytalo.com `/docs/sockets/responses` (canonical types: `Prop`,
`Player`, `PlayerAlias`, `GameChannel`, `GameChannelLeavingReason`),
`/docs/http/leaderboard-api` (entry samples: float score, 0-based
`position`, nested alias *without* timestamps), and
`/docs/http/game-channel-api` (`owner: null` for system channels,
`autoCleanup` / `private` flags, alias payloads *with* timestamps).

**Decisions:**
- New modules under `src/entities/`: `prop.rs` (shared key/value pair),
  `player_auth.rs`, `player_alias.rs` (with nested `PlayerRef`),
  `game_channel.rs`; `leaderboard.rs` gains `LeaderboardSortMode` plus a full
  upstream-parity `LeaderboardEntry` **alongside** — never replacing — our
  implemented `LeaderboardEntryView`, so wire compatibility is untouched.
- `GameChannel.owner` uses the full `Option<PlayerAlias>` rather than a
  summary struct: live samples show owner is a complete alias and there is
  no recursion cycle (alias → player → auth), so full parity costs nothing.
- Alias-level timestamps are `Option<DateTime<Utc>>`: channel payloads carry
  them but leaderboard-entry samples omit them; liberal acceptance keeps
  both real fixtures deserializable.
- `LeaderboardEntry.score` is `f64` (upstream samples are floats, e.g.
  `593.21`) while our persisted schema stays `BIGINT i64`; conversion
  belongs to the future service layer. `position: u64` mirrors upstream's
  0-based index; our views keep their 1-based `rank`.
- `GameChannelLeavingReason` gets hand-written integer serde: serde variant
  `rename` would emit JSON strings (`"0"`), upstream emits bare integers.
  Matches the manual-decode precedent set by `PlayerStatus`.
- Upstream's circular `Player.aliases` back-reference is omitted;
  `Player.groups` is not modelled until player-group persistence exists.

**Consequences:**
- Purely additive data structs; no handler/service/socket changes, so every
  existing response shape stays byte-compatible.
- Security invariant pinned in unit tests: neither `PlayerAuth` nor
  `PlayerAlias` can serialize credential material (`password_hash` remains
  on the internal `players` row only).
- When channel/analytics endpoints land later in 0.4, these structs are the
  ready-made parity targets instead of new ad-hoc shapes.

---

## 2026-08-26 — `game_settings`: opaque route id, no FK until a games table exists (task 0.4.3)

**Context:** task 0.4.3 adds the `game_settings` table backing
`GET/PUT /v1/games/{game_id}/settings`. Upstream addresses games by an opaque
route id, but Playzoid v0 has no `games` table yet, so the column cannot
foreign-key anywhere.

**Options considered:**
- Wait for a `games` table and add the FK then — blocks settings work behind
  an unscoped task.
- Invent a minimal `games` table in this task — scope creep beyond owned
  paths (`migrations/`) and guesses at a schema no task specifies.
- Store the route id directly with a unique constraint, no FK (chosen).

**Decision:** `game_id VARCHAR(64) NOT NULL UNIQUE` holds the opaque route
identifier, mirroring the leaderboards' `internal_name` convention; the
internal BIGINT id never leaves the server (players/game_saves precedent).
`config JSON NOT NULL` carries arbitrary per-game configuration, one row per
game via the unique constraint. Size capping is deliberately not in-schema —
MySQL JSON columns cannot be size-bounded — and belongs to the API layer
(task 0.4.4), same split as the leaderboards' props ≤ 4 KB rule.

**Consequences:**
- When a `games` table lands, adding the FK is a small follow-up migration;
  existing rows already key on route ids.
- PUT upsert semantics live in task 0.4.4.

---

## 2026-08-26 — Game settings endpoints: Playzoid extension, JWT-only writes, 32 KiB cap, upsert (task 0.4.4)

**Context:** task 0.4.4 adds `GET/PUT /v1/games/{game_id}/settings`. Upstream
Talo has only dashboard-managed global `GET /v1/game-config` KV — verified no
upstream per-game settings HTTP endpoint exists, so the task text plus the
0.4.3 schema decision are authoritative for the shape.

**Decisions:**
- **Documented Playzoid extension**, not upstream parity: request/response
  shape defined by this repo (`{ gameId, config, createdAt, updatedAt }`,
  camelCase like every other view). Recorded here so 0.4.13's TALO_API.md
  update marks it as an extension rather than upstream surface.
- **PUT upserts** (`INSERT … ON DUPLICATE KEY UPDATE config = ?`, fully
  parameterized, value bound twice): first PUT creates, later PUTs replace
  only `config`; `created_at` preserved, `updated_at` maintained by MySQL.
  Always returns 200 with the read-back view — no create/update status split,
  because clients address the row by id, not by server-assigned key.
- **Size cap 32 KiB** (`MAX_CONFIG_BYTES`), mirroring the saves
  `MAX_SAVE_BYTES` precedent; enforced pre-SQL alongside null-config and
  id-length validation so bad requests never touch the database.
- **No legacy alias mount**: the route is born after the 0.4.1 parity pass;
  only `/v1/games/{game_id}/settings` exists (`/v1/socket-tickets`
  precedent).
- **Auth trade-off:** both endpoints require a valid JWT, but there is no
  `games` table / ownership / admin scope yet, so any authenticated player
  can overwrite any game's config. v0 accepts this per the task spec
  ("auth-guarded"); revisit when `games` or scopes land. Rate limiting
  arrives in 0.4.8.

---

## 2026-08-26 — Analytics events: append-only, SET NULL player FK, generic name+JSON schema (task 0.4.5)

**Context:** task 0.4.5 adds the `analytics_events` table backing the batched
`POST /v1/events` ingest (0.4.6). Upstream Talo's analytics event shape is
undocumented in this repo (`TALO_API.md` has no events section;
`TALO_API_STRUCTS.md` has no Event struct) — verified by grep before
designing. `docs/memory.md` Open Question #6 already pins typed event
schema as deferred to Phase 1.0.

**Decisions:**
- **Append-only semantics encoded in schema:** no `updated_at` column
  (rows are never updated; its omission documents immutability intent)
  and no `public_id` (unlike saves/players, events are write-only
  fire-and-forget in v0 — clients never address a stored event).
- **`player_id` nullable + ON DELETE SET NULL**, not CASCADE: events may be
  emitted pre-identify (anonymous), and deleting a player must never erase
  their event history — CASCADE would silently rewrite history and break
  append-only guarantees.
- **Generic schema:** free-form `name VARCHAR(64)` event key + optional JSON
  `props`. No upstream-specific columns guessed from an undocumented shape;
  whatever 0.4.6's batched body needs fits. Typed enum/payload schema lands
  in Phase 1.0 when upstream shapes are verified.
- **Minimal indexing for a high-write log:** `(player_id)` for per-player
  queries, `(name, created_at)` for per-event-type time-range queries. No
  more — every index taxes ingest throughput.

---

## Open Questions / Assumptions

These mirror the open questions in [`docs/todo.md`](todo.md). Resolve
them here when answered.

1. **Database engine** — MySQL is canonical (see `migrations/` and the sqlx
   `mysql` feature flag). PostgreSQL parity is *out of scope* until 1.0.0.
2. **JWT secret rotation** — static env-var secret for v1 (see `Config::from_env`
   validation in `src/config.rs`). Revisit after 1.0.0.
3. **Talo TypeScript reference** — see `docs/TALO_API.md` for the verified
   subset of upstream shapes; gaps tracked under "Remaining TODOs" in that file.
4. **Auth identifier** — `identifier` (matches Talo upstream — accepts
   email | username | numeric id). `username` alone is *not* canonical.
5. **Subaccount limits** — *open*; pending product input.
6. **Analytics event schema** — *open*; deferred to Phase 1.0.
