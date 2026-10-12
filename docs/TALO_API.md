<!-- Verified & updated by Sonnet subagent — maps to TaloDev/backend (extracted from src/routes and src/socket) -->
# TALO_API.md — Talo Rust Server API Reference (verified)

This file documents verified API shapes for implementing a Talo-compatible Rust backend. It was updated by inspecting https://github.com/TaloDev/backend and mapping Zod schemas and socket listeners to Rust serde types.

Style note: To match the TypeScript shapes, prefer camelCase JSON keys. In Rust use serde(rename_all = "camelCase") on structs.

---

## HTTP API: auth (v1/players/auth)

### POST /v1/players/auth/login
Request body (verified):
```json
{
  "identifier": "player@example.com | username | numeric-id",
  "password": "plaintext",
  "withRefresh": true // optional
}
```
Response (200) — when verification disabled:
```json
{
  "alias": { /* PlayerAlias object (see Player/alias models) */ },
  "sessionToken": "<token>",
  "refreshToken": "<token?>",
  "socketToken": "<socket-ticket>"
}
```
When verification is enabled, response may be:
```json
{
  "aliasId": 123,
  "verificationRequired": true
}
```
Rust serde (example):
```rust
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub identifier: String,
    pub password: String,
    pub with_refresh: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponseAlias {
    // partial alias shape — extend as needed
    pub id: i64,
    pub identifier: String,
    pub player_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponse {
    pub alias: Option<LoginResponseAlias>,
    pub session_token: Option<String>,
    pub refresh_token: Option<String>,
    pub socket_token: Option<String>,
}
```

Notes: fields and names (identifier/sessionToken/socketToken) are taken directly from Talo TS handlers (login.ts, register.ts). Use the full alias/player models when implementing persistence.

---

## HTTP API: players (/v1/players)

Talo uses `Player` + `PlayerAlias` domain. Common endpoints (verified):
- GET /v1/players/:id — returns Player (see TS `routes/api/player/get.ts`)
- POST /v1/players/register — handled under /v1/players/auth/register in Talo (see register.ts)
- POST /v1/players/socket-token — returns a short-lived socket token for WS auth

Example create/login shapes already covered in auth section. The `Player`/`Alias` objects are large; prefer implementing minimal fields required by your client and expand as needed.

Example Player serde (partial):
```rust
#[derive(Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub id: String,
    pub username: Option<String>,
    pub email: Option<String>,
    pub status: Option<String>,
}
```

---

## Domain models (upstream parity)

Full serde structs for the complex upstream domain types, verified against
docs.trytalo.com (`/docs/sockets/responses`, `/docs/http/leaderboard-api`,
`/docs/http/game-channel-api`) and lifted into `src/entities/` (task 0.4.2).

### `Prop` — `src/entities/prop.rs`

```ts
type Prop = { key: string, value: string }
```

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prop {
    pub key: String,
    pub value: String,
}
```

### `PlayerAuth` — `src/entities/player_auth.rs`

Upstream nests this as the optional `Player.auth?` block. Playzoid keeps
`password_hash` on the internal `players` row only; no auth struct ever
carries credential material (pinned by a unit test).

```ts
type PlayerAuth = {
  email?: string
  verificationEnabled: boolean
  sessionCreatedAt?: Date
}
```

```rust
pub struct PlayerAuth {
    pub email: Option<String>,
    #[serde(default)]
    pub verification_enabled: bool,
    #[serde(default)]
    pub session_created_at: Option<DateTime<Utc>>,
}
```

### `PlayerAlias` (+ nested `PlayerRef`) — `src/entities/player_alias.rs`

Verified upstream shape (socket reference type + game-channel API owner /
member samples):

```ts
type PlayerAlias = {
  id: number
  service: string
  identifier: string
  displayName?: string
  player: Player
  lastSeenAt?: Date
  createdAt?: Date
  updatedAt?: Date
}

type Player = {
  id: string            // public UUID on every verified HTTP sample
  props: Prop[]
  devBuild: boolean
  lastSeenAt: Date
  createdAt: Date
  groups?: { id: number, name: string }[]
  auth?: PlayerAuth
}
```

Rust mapping:

```rust
pub struct PlayerRef {
    pub id: String,
    #[serde(default)] pub props: Vec<Prop>,
    pub dev_build: bool,
    pub last_seen_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    #[serde(default)] pub auth: Option<PlayerAuth>,
}

pub struct PlayerAlias {
    pub id: i64,
    pub service: String,
    pub identifier: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub display_name: Option<String>,
    pub player: PlayerRef,
    #[serde(default)] pub last_seen_at: Option<DateTime<Utc>>,
    #[serde(default)] pub created_at: Option<DateTime<Utc>>,
    #[serde(default)] pub updated_at: Option<DateTime<Utc>>,
}
```

Divergences (documented, deliberate):

| Upstream field | Handling | Why |
|----------------|----------|-----|
| `Player.aliases` | Omitted | Circular back-reference (`[Circular]` in every upstream sample) |
| `Player.groups` | Not modelled yet | Playzoid has no player-group persistence |
| Alias-level timestamps `Option` | Liberal deserialize | Channel payloads carry them; leaderboard-entry samples omit them |
| Socket-reference `Player.id: number` vs HTTP UUID string | Modelled as `String` | Every verified HTTP sample uses the UUID |

### `GameChannel` — `src/entities/game_channel.rs`

```ts
type GameChannel = {
  id: number
  name: string
  owner: PlayerAlias | null   // null for system channels (verified)
  totalMessages: number
  memberCount: number
  props: Prop[]
  autoCleanup: boolean        // HTTP payloads; absent in socket fan-outs
  private: boolean            // HTTP payloads; absent in socket fan-outs
  createdAt: Date
  updatedAt: Date
}
```

Rust mapping (`owner` is the full [`PlayerAlias`] — not recursive):

```rust
pub struct GameChannel {
    pub id: i64,
    pub name: String,
    pub owner: Option<PlayerAlias>,
    pub total_messages: i64,
    pub member_count: i64,
    #[serde(default)] pub props: Vec<Prop>,
    #[serde(default)] pub auto_cleanup: bool,
    #[serde(rename = "private", default)] pub is_private: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

The `v1.channels.player-left` envelope carries `meta.reason` as an integer
(`DEFAULT = 0`, `TEMPORARY_MEMBERSHIP = 1`); modelled as
`GameChannelLeavingReason` with hand-written integer serde (variant `rename`
would emit strings). Playzoid's in-memory channel hub currently emits only
reason `0`.

### Full `LeaderboardEntry` — `src/entities/leaderboard.rs`

Verified against the leaderboard API samples:

```ts
type LeaderboardEntry = {
  id: number
  position: number              // 0-based
  score: number                 // float in upstream samples (593.21)
  leaderboardName: string
  leaderboardInternalName: string
  leaderboardSortMode: "asc" | "desc"
  playerAlias: PlayerAlias
  hidden: boolean
  props?: Prop[]
  createdAt: Date
  updatedAt: Date
}
```

Rust mapping (alongside — not replacing — our implemented
`LeaderboardEntryView` / `LeaderboardResponse`):

```rust
pub enum LeaderboardSortMode { Asc, Desc } // lowercase wire form

pub struct LeaderboardEntry {
    pub id: i64,
    pub position: u64,
    pub score: f64, // upstream parity; our BIGINT column stays i64
    pub leaderboard_name: String,
    pub leaderboard_internal_name: String,
    pub leaderboard_sort_mode: LeaderboardSortMode,
    pub player_alias: PlayerAlias,
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<Prop>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

Divergences vs our implemented surface: upstream exposes a 0-based
`position` while Playzoid views expose a 1-based `rank`; upstream scores are
numeric floats while our schema stores `BIGINT`. Neither migration nor the
implemented view changes here.

---

## Playzoid-Server Implemented Endpoints (Phases 0.2 & 0.3)

The sections above document the upstream **Talo TypeScript** API shapes for reference.
The sections below document what is **actually implemented** in this server.

Route prefixes: **canonical paths carry the `/v1` prefix** (`/v1/auth`, `/v1/players`,
`/v1/leaderboards`, `/v1/saves`) since task 0.4.1; the legacy unprefixed
(`/auth`, `/players`, `/leaderboards`, `/saves`) mounts remain as aliases during the
transition and share the same handlers. The WebSocket lives at `/ws`.
Auth scheme: `Authorization: Bearer <token>` on all protected routes.  
Content-Type: `application/json` for all requests and responses.

Rate limiting (task 0.4.8): public-route budgets are enforced per client IP;
exceeded requests answer `429 Too Many Requests` with `Retry-After` and
`X-RateLimit-Limit/Remaining/Reset` headers. `/healthz`, `/metrics` and
`/openapi.json` are never limited.

An OpenAPI 3.0 document is served at `GET /openapi.json` (task 0.4.10) and is the
authoritative machine-readable route reference; CI fails on drift. Prometheus
metrics are served at `GET /metrics` (task 0.4.9).

### Common types

#### `PlayerView` — public player projection

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "username": "alice",
  "email": "alice@example.com",
  "parent_account_id": null,
  "status": "active",
  "created_at": "2026-05-27T12:00:00Z"
}
```

Rust struct (from `src/entities/player.rs`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerView {
    pub id: String,                        // public_id (UUID)
    pub username: String,
    pub email: Option<String>,
    pub parent_account_id: Option<String>, // parent's public_id; null for root accounts
    pub status: PlayerStatus,              // "active" | "suspended" | "deleted"
    pub created_at: DateTime<Utc>,
}
```

`status` is serialized as a lowercase string: `"active"`, `"suspended"`, or `"deleted"`.

#### Error envelope

All error responses use a flat string envelope:

```json
{ "error": "human-readable message" }
```

---

### POST /auth/register

Create a new root-level player account. No authentication required.

**Request body:**

```json
{
  "username": "alice",
  "email": "alice@example.com",
  "password": "supersecret1",
  "parent_account_id": null
}
```

| Field | Type | Required | Constraints |
|-------|------|----------|-------------|
| `username` | string | yes | 3–64 characters |
| `email` | string | no | valid email format |
| `password` | string | yes | 8–1024 characters |
| `parent_account_id` | string (UUID) | no | public_id of the parent player |

**Response 201 — Created:**

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "username": "alice",
  "email": "alice@example.com",
  "parent_account_id": null,
  "status": "active",
  "created_at": "2026-05-27T12:00:00Z"
}
```

| Status | Meaning |
|--------|----------|
| 201 | Account created; body is `PlayerView` |
| 400 | Validation error (`error` field describes which rule failed) |
| 409 | Username or email already taken |
| 503 | Database unavailable |

---

### POST /auth/login

Authenticate with username + password; returns a signed JWT.

**Request body:**

```json
{
  "username": "alice",
  "password": "supersecret1"
}
```

| Field | Type | Required | Constraints |
|-------|------|----------|-------------|
| `username` | string | yes | 3–64 characters |
| `password` | string | yes | 1–1024 characters |

**Response 200 — OK:**

```json
{
  "token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
  "expires_in": 3600,
  "player": {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "username": "alice",
    "email": "alice@example.com",
    "parent_account_id": null,
    "status": "active",
    "created_at": "2026-05-27T12:00:00Z"
  }
}
```

Rust struct (from `src/api/auth.rs`):

```rust
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_in: u64,   // seconds; sourced from Config::jwt_expiry_secs
    pub player: PlayerView,
}
```

| Status | Meaning |
|--------|----------|
| 200 | Login successful; `token` is a HS256 JWT signed with `JWT_SECRET` |
| 400 | Validation error |
| 401 | Invalid credentials |
| 503 | Database unavailable |

The JWT `sub` claim contains the player's `public_id`.  
On success the player view is also written to the Redis cache with a TTL of `expires_in` seconds.

---

### GET /players/{id}

Fetch a player's public profile by `public_id`. Any authenticated user may read any player.

**Auth:** `Authorization: Bearer <token>` required.

**Path parameter:** `id` — player's `public_id` (UUID string).

**Response 200 — OK:** `PlayerView` (see type above).

The handler checks Redis first; on a miss it queries MySQL and back-fills the cache.

| Status | Meaning |
|--------|----------|
| 200 | Player found |
| 401 | Missing or invalid token |
| 404 | No player with that `public_id` |
| 503 | Database unavailable |

---

### PUT /players/{id}

Update the authenticated player's own profile. You may only update your own account.

**Auth:** `Authorization: Bearer <token>` required.

**Path parameter:** `id` — target player's `public_id` (must equal the authenticated user's id).

**Request body** (all fields optional; send only what should change):

```json
{
  "username": "new_alice",
  "email": "newalice@example.com"
}
```

| Field | Type | Required | Constraints |
|-------|------|----------|-------------|
| `username` | string | no | 3–64 characters |
| `email` | string | no | valid email format |

**Response 200 — OK:** Updated `PlayerView`.

On success the Redis cache entry is invalidated.

| Status | Meaning |
|--------|----------|
| 200 | Updated; body is new `PlayerView` |
| 400 | Validation error |
| 401 | Missing or invalid token |
| 403 | Attempting to update a different player's account |
| 404 | Player not found |
| 409 | New username or email already taken |
| 503 | Database unavailable |

---

### DELETE /players/{id}

Soft-delete the authenticated player's own account. The row is retained for FK integrity; `status` is set to `"deleted"` and `deleted_at` is stamped.

**Auth:** `Authorization: Bearer <token>` required.

**Path parameter:** `id` — target player's `public_id` (must equal the authenticated user's id).

**Response 204 — No Content** (empty body on success).

On success the Redis cache entry is invalidated.

| Status | Meaning |
|--------|----------|
| 204 | Soft-delete applied |
| 401 | Missing or invalid token |
| 403 | Attempting to delete a different player's account |
| 404 | Player not found |
| 503 | Database unavailable |

---

### POST /players/subaccount

Create a new subaccount linked to the authenticated player as its parent. The parent relationship is inferred from the JWT — the caller cannot specify an arbitrary parent.

**Auth:** `Authorization: Bearer <token>` required.

**Request body:**

```json
{
  "username": "child_account",
  "email": "child@example.com",
  "password": "childpass1"
}
```

| Field | Type | Required | Constraints |
|-------|------|----------|-------------|
| `username` | string | yes | 3–64 characters |
| `email` | string | no | valid email format |
| `password` | string | yes | 8–1024 characters |

**Response 201 — Created:** `PlayerView` with `parent_account_id` populated to the authenticated player's `public_id`.

```json
{
  "id": "661f9511-f3ac-52e5-b827-557766551111",
  "username": "child_account",
  "email": "child@example.com",
  "parent_account_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "active",
  "created_at": "2026-05-27T12:05:00Z"
}
```

| Status | Meaning |
|--------|----------|
| 201 | Subaccount created; body is `PlayerView` |
| 400 | Validation error |
| 401 | Missing or invalid token |
| 409 | Username or email already taken |
| 503 | Database unavailable |

---

### GET /players/{id}/subaccounts

List all non-deleted subaccounts for the given parent player. Only the parent may list their own subaccounts.

**Auth:** `Authorization: Bearer <token>` required.

**Path parameter:** `id` — parent player's `public_id` (must equal the authenticated user's id).

**Response 200 — OK:** Array of `PlayerView`. Empty array when no subaccounts exist.

```json
[
  {
    "id": "661f9511-f3ac-52e5-b827-557766551111",
    "username": "child_account",
    "email": null,
    "parent_account_id": "550e8400-e29b-41d4-a716-446655440000",
    "status": "active",
    "created_at": "2026-05-27T12:05:00Z"
  }
]
```

| Status | Meaning |
|--------|----------|
| 200 | OK; may be empty array |
| 401 | Missing or invalid token |
| 403 | Attempting to list another player's subaccounts |
| 404 | Parent player not found |
| 503 | Database unavailable |

---

## HTTP API: leaderboards (implemented, Phase 0.3)

Auth: `Authorization: Bearer <token>` required on every route.
`{game_id}` is the leaderboard's route identifier (`internal_name`).

Routes:

| Method | Path                                                       | Purpose                     |
|--------|------------------------------------------------------------|-----------------------------|
| GET    | `/leaderboards/{game_id}`                                  | Paginated ranked top scores |
| POST   | `/leaderboards/{game_id}/entries`                          | Submit a score (own player) |
| PUT    | `/leaderboards/{game_id}/entries/{player_id}`              | Update own score            |

### GET /leaderboards/{game_id}

Query params:

| Param      | Type | Default | Constraints   |
|------------|------|---------|---------------|
| `page`     | int  | 1       | 1-based, >= 1 |
| `per_page` | int  | 50      | 1..=100       |

Response 200 — ranked entries, highest score first (soft-deleted players excluded):

```json
{
  "entries": [
    { "playerId": "550e8400-e29b-41d4-a716-446655440000", "score": 1000, "rank": 1 },
    { "playerId": "661f9511-f3ac-52e5-b827-557766551111", "score": 950,  "rank": 2 }
  ]
}
```

Ranks are 1-based and continue across pages (page 2 starts at `per_page + 1`). Ties are
broken by earliest submission.

Rust structs (from `src/entities/leaderboard.rs`):

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaderboardEntryView {
    pub player_id: String, // public UUID of the owning player
    pub score: i64,
    pub rank: u64,         // 1-based; continues across pages
}

#[derive(Debug, Clone, Serialize)]
pub struct LeaderboardResponse {
    pub entries: Vec<LeaderboardEntryView>,
}
```

| Status | Meaning |
|--------|---------|
| 200 | Page of ranked entries (may be `entries: []`) |
| 400 | Invalid pagination (`page >= 1`, `1 <= per_page <= 100`) |
| 401 | Missing or invalid token |
| 404 | Unknown `{game_id}` |
| 503 | Database unavailable |

### POST /leaderboards/{game_id}/entries

The owning player is taken from the JWT — callers cannot submit on behalf of another
player. One entry per player per leaderboard (re-submission → 409; use PUT to update).

Request body (`deny_unknown_fields`):

```json
{ "score": 1000, "props": [ { "level": 3 } ] }
```

| Field   | Type        | Required | Constraints                  |
|---------|-------------|----------|------------------------------|
| `score` | int         | yes      | `i64`                        |
| `props` | JSON array  | no       | ≤ 4096 bytes when serialised |

Response 201:

```json
{ "playerId": "550e8400-e29b-41d4-a716-446655440000", "score": 1000, "rank": 1, "props": [ { "level": 3 } ] }
```

| Status | Meaning |
|--------|---------|
| 201 | Entry stored; rank = 1 + COUNT(higher scores or equal scores submitted earlier) |
| 400 | Invalid body (`props` not an array / oversize) |
| 401 | Missing or invalid token |
| 404 | Unknown `{game_id}` or player |
| 409 | Entry for this player already exists on this leaderboard |
| 503 | Database unavailable |

### PUT /leaderboards/{game_id}/entries/{player_id}

`{player_id}` must match the JWT identity (403 otherwise). The entry must already exist
(404; use POST to create one). Omitted `props` keep their current value. Response 200
with `{ playerId, score, rank, props? }`, rank recomputed.

| Status | Meaning |
|--------|---------|
| 200 | Entry updated; rank recomputed |
| 400 | Invalid body |
| 401 | Missing or invalid token |
| 403 | Cross-player update attempt |
| 404 | Unknown `{game_id}`, unknown player, or no entry for this player |
| 503 | Database unavailable |

Rust request struct (from `src/api/leaderboards.rs`, shared by POST and PUT):

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitScoreRequest {
    pub score: i64,
    #[serde(default)]
    pub props: Option<serde_json::Value>,
}
```

---

## HTTP API: game saves (implemented, Phase 0.3)

Auth: `Authorization: Bearer <token>` required on every route. Saves are private
per-player game state — unlike profile reads, these endpoints only ever touch the
caller's own saves (`{player_id}` must match the JWT identity, else 403).

Routes:

| Method | Path                            | Purpose                       |
|--------|---------------------------------|-------------------------------|
| POST   | `/saves`                        | Create a game save            |
| GET    | `/saves/{player_id}`            | List own saves (newest first) |
| GET    | `/saves/{player_id}/{save_id}`  | Retrieve one save             |
| DELETE | `/saves/{player_id}/{save_id}`  | Delete one save               |

### Common types

`SaveView` — public projection. The internal BIGINT id is never exposed; the save's
`public_id` (UUID) is surfaced as `id` and the owning player's `public_id` as `playerId`.

```json
{
  "id": "b7e9c1f2-0000-4000-8000-000000000001",
  "playerId": "550e8400-e29b-41d4-a716-446655440000",
  "name": "slot-1",
  "save": { "hp": 100, "level": 3 },
  "metadata": { "zone": "level1" },
  "createdAt": "2026-08-25T12:00:00Z",
  "updatedAt": "2026-08-25T12:00:00Z"
}
```

Rust struct (from `src/entities/save.rs`):

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveView {
    pub id: String,                    // save's public_id (UUID)
    pub player_id: String,             // owning player's public_id (UUID)
    pub name: String,
    pub save: serde_json::Value,       // arbitrary game-state blob
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

### POST /saves

`playerId` is optional: absent, it defaults to the JWT identity; present, it must match
(403 otherwise) — keeps the own-only property while staying compatible with Talo-shaped
clients.

Request body (`deny_unknown_fields`):

```json
{
  "name": "slot-1",
  "playerId": "550e8400-e29b-41d4-a716-446655440000",
  "save": { "hp": 100 },
  "metadata": { "zone": "level1" }
}
```

| Field      | Type        | Required | Constraints                                          |
|------------|-------------|----------|------------------------------------------------------|
| `name`     | string      | yes      | 1..=255 characters                                   |
| `playerId` | string      | no       | must equal the JWT identity when supplied            |
| `save`     | JSON value  | yes      | must not be JSON `null`                              |
| `metadata` | JSON value  | no       | `save` + `metadata` combined ≤ 32 KiB when serialised |

Response 201 — full `SaveView`.

| Status | Meaning |
|--------|---------|
| 201 | Save created; body is `SaveView` |
| 400 | Validation error (name length, null `save`, oversize blob, unknown fields) |
| 401 | Missing or invalid token |
| 403 | `playerId` does not match the JWT identity |
| 404 | Unknown or soft-deleted player |
| 503 | Database unavailable |

Rust request struct (from `src/api/saves.rs`):

```rust
#[derive(Debug, Deserialize, Validate)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSaveRequest {
    #[validate(length(min = 1, max = 255))]
    pub name: String,
    pub player_id: Option<String>,
    pub save: serde_json::Value,
    pub metadata: Option<serde_json::Value>,
}
```

### GET /saves/{player_id}

List the authenticated player's saves, newest first (`created_at DESC, updated_at DESC`).
Response 200 — array of `SaveView` (empty array when no saves exist).

| Status | Meaning |
|--------|---------|
| 200 | Saves listed (may be `[]`) |
| 401 | Missing or invalid token |
| 403 | Cross-player read attempt |
| 404 | Unknown or soft-deleted player |
| 503 | Database unavailable |

### GET /saves/{player_id}/{save_id}

Retrieve one save, scoped to the owning player's internal id — an unknown `{save_id}`,
or one owned by a different player, returns 404 (never leaks). Response 200 — `SaveView`.

| Status | Meaning |
|--------|---------|
| 200 | Save retrieved; body is `SaveView` |
| 401 | Missing or invalid token |
| 403 | Cross-player read attempt |
| 404 | Unknown player or save |
| 503 | Database unavailable |

### DELETE /saves/{player_id}/{save_id}

Delete one save, scoped to the owning player's internal id; unknown or cross-player
save ids affect zero rows and return 404 (never leaks). Response 204 — empty body.

| Status | Meaning |
|--------|---------|
| 204 | Save deleted |
| 401 | Missing or invalid token |
| 403 | Cross-player delete attempt |
| 404 | Unknown player or save |
| 503 | Database unavailable |

---

## HTTP API: game settings (implemented, Phase 0.4)

Auth: `Authorization: Bearer <token>` required on every route. A **Playzoid
extension** — upstream Talo has no per-game settings endpoint (documented in
`docs/memory.md`). `{game_id}` is the opaque route identifier (1..=64 chars).

| Method | Path                            | Purpose                       |
|--------|---------------------------------|-------------------------------|
| GET    | `/v1/games/{game_id}/settings`  | Fetch a game's settings       |
| PUT    | `/v1/games/{game_id}/settings`  | Set a game's settings (upsert)|

### GET /v1/games/{game_id}/settings

Response 200 — `GameSettingView`:

```json
{ "gameId": "my-game", "config": { "playerSpeed": 1.5 }, "createdAt": "2026-08-26T00:00:00Z" }
```

| Status | Meaning |
|--------|---------|
| 200 | Settings returned |
| 401 | Missing or invalid token |
| 404 | No settings row for `{game_id}` |
| 503 | Database unavailable |

### PUT /v1/games/{game_id}/settings

Request body (`deny_unknown_fields`):

```json
{ "config": { "playerSpeed": 1.5 } }
```

| Field    | Type       | Required | Constraints            |
|----------|------------|----------|------------------------|
| `config` | JSON value | yes      | ≤ 32 KiB, non-null     |

`{game_id}` is trimmed symmetrically on read-back so GET addresses the exact row a PUT
created. Response 200 — `GameSettingView` (`created_at` preserved on update).

| Status | Meaning |
|--------|---------|
| 200 | Settings upserted; body is `GameSettingView` |
| 400 | Validation error (missing config, oversize blob, `{game_id}` > 64 chars, unknown fields) |
| 401 | Missing or invalid token |
| 404 | Unknown or soft-deleted player |
| 503 | Database unavailable |

---

## HTTP API: analytics events (implemented, Phase 0.4)

Auth: `Authorization: Bearer <token>` required.

### POST /v1/events

Ingest a batch of analytics events. **Fire-and-forget**: accepted batches answer
`202 {"accepted": n}` even if the post-validation insert fails (logged server-side) —
telemetry loss never blocks clients.

Body is a bare JSON array (no wrapper object, no client timestamps — `created_at` is
DB-stamped), validated whole-batch before any SQL:

```json
[ { "name": "level_started", "props": { "level": 3 } } ]
```

| Constraint | Value |
|------------|-------|
| Batch size | 1..=100 events |
| `name`     | trimmed, 1..=64 chars (matches `VARCHAR(64)`) |
| `props`    | optional JSON; ≤ 4 KiB when serialised |
| Unknown fields | rejected per event (`deny_unknown_fields`) |

Attribution is best-effort: an unknown/deleted caller stores anonymous rows
(`player_id NULL`) rather than failing.

| Status | Meaning |
|--------|---------|
| 202 | Batch accepted (`{"accepted": n}`) |
| 400 | Whole-batch validation failure (nothing written) |
| 401 | Missing or invalid token |
| 503 | Database unavailable (no pool) |

---

## HTTP API: feedback (implemented, Phase 0.4)

Auth: `Authorization: Bearer <token>` required.

### POST /v1/feedback

Submit player feedback. Stored as `name = "feedback"` rows in the append-only
`analytics_events` table (sink-reuse decision, `docs/memory.md`). Unlike events this
is **honest-failure**: a post-validation DB failure answers `500` (details logged
server-side only) — user content is never silently dropped.

Request body (`deny_unknown_fields`):

```json
{ "message": "Great game, one nitpick though" }
```

| Field     | Type   | Required | Constraints                                          |
|-----------|--------|----------|------------------------------------------------------|
| `message` | string | yes      | trimmed 1..=1000 chars; JSON-encoded ≤ 4 KiB         |

Response 201 — `{"received": true}`.

| Status | Meaning |
|--------|---------|
| 201 | Feedback stored (`{"received": true}`) |
| 400 | Validation failure (nothing written) |
| 401 | Missing or invalid token |
| 500 | Post-validation DB failure |
| 503 | Database unavailable (no pool) |

Best-effort attribution identical to events: unknown/deleted callers store anonymous
rows rather than failing.

---

## HTTP API: rate limiting (implemented, Phase 0.4)

Enforced by global middleware on public routes (`/v1/auth/**`, `/auth/**` with a tight
budget; other configured public prefixes with a general budget). Buckets are
Redis fixed windows, one key per `(class, client ip, window_start)`.

Blocked responses (429):

```json
{ "error": "rate limit exceeded" }
```

Headers: `Retry-After`, `X-RateLimit-Limit`, `X-RateLimit-Remaining`,
`X-RateLimit-Reset`.

Degraded mode is fail-open: Redis down at boot (no limiter app data) or a mid-flight
backend error passes the request through. `X-Forwarded-For` trust is opt-in
(`RATE_LIMIT_TRUST_XFF=true`) — the default keys on the socket peer IP.

---

## WebSocket protocol (implemented, Phase 0.3)

Connection is via a WebSocket upgrade to `/ws`, authenticated by a one-shot socket
ticket passed as the `?ticket=` query parameter (matching the Talo flow).

### Socket tickets

`POST /v1/socket-tickets` — issue a one-shot ticket bound to a player alias. The ticket
store is in-memory; the bound alias is the authenticated identity for the resulting
socket. No auth required on this route.

Request:
```json
{ "alias_id": 42 }
```

Response 200:
```json
{ "ticket": "b7e9c1f2-0000-4000-8000-000000000002" }
```

### Connect

```
GET /ws?ticket=<socket-ticket>
```

A missing or invalid ticket still upgrades the connection so the client receives the
Talo error envelope, then the socket is closed with a policy close code:

```json
{ "res": "v1.error", "data": { "code": "INVALID_SOCKET_TOKEN", "message": "Missing or invalid socket ticket" } }
```

### Envelope format

Client → Server requests use a `req` string plus a `data` object; Server → Client
responses use a `res` string plus a `data` object.

```rust
pub struct SocketRequestEnvelope { pub req: String, pub data: serde_json::Value }
pub struct SocketResponseEnvelope<T: Serialize> { pub res: String, pub data: T }
```

### Client → Server requests

| `req`                    | `data`                                   | Notes                                                     |
|--------------------------|------------------------------------------|-----------------------------------------------------------|
| `v1.players.identify`    | `{ "playerAliasId": 42 }`                | Must equal the ticket-bound alias (else `INVALID_INPUT`)  |
| `v1.channels.join`       | `{ "channelId": 7 }`                     | Playzoid request extension (see note below)               |
| `v1.channels.leave`      | `{ "channelId": 7 }`                     | Playzoid request extension (see note below)               |
| `v1.channels.message`    | `{ "channelId": 7, "message": "hi" }`    | Message 1..=1000 chars; Playzoid request extension        |
| `v1.heartbeat`           | (bare token, not an envelope)            | Echoed verbatim as `v1.heartbeat`                         |

`v1.channels.join` / `v1.channels.leave` / `v1.channels.message` must be preceded by a
successful `v1.players.identify`, and all channel frames use the **ticket-bound alias**
for membership and sending — a client-supplied `playerAliasId` claim is ignored
(spoof-proof). Unknown request strings → `UNHANDLED_REQUEST`; malformed JSON →
`INVALID_JSON`.

### Server → Client responses

`v1.players.identify.success`:
```json
{ "res": "v1.players.identify.success", "data": { "aliasId": 42, "playerId": "player-42", "parentAccountId": null } }
```
`playerId` is a server-side projection (`player-{aliasId}`); `parentAccountId` is the
server-resolved subaccount parent (`null` for root accounts or when the DB lookup
degrades).

`v1.players.presence.updated` — fanned out to every connected socket when a player comes
online (first identified connection for an alias) or goes offline (last connection
drops):
```json
{
  "res": "v1.players.presence.updated",
  "data": {
    "presence": {
      "playerAlias": { "id": 42 },
      "online": true,
      "customStatus": null,
      "lastSeenAt": "2026-08-25T12:00:00Z"
    },
    "meta": { "onlineChanged": true, "customStatusChanged": false }
  }
}
```

`v1.channels.player-joined` / `v1.channels.player-left` — fanned out to channel members
when a participant group's first/last connection joins/leaves:
```json
{ "res": "v1.channels.player-joined", "data": { "channel": { "id": 7 }, "playerAlias": { "id": 42 } } }
{ "res": "v1.channels.player-left", "data": { "channel": { "id": 7 }, "playerAlias": { "id": 42 }, "meta": { "reason": 0 } } }
```

`v1.channels.message` — chat broadcast to every member connection, sender included:
```json
{ "res": "v1.channels.message", "data": { "channel": { "id": 7 }, "message": "hi", "playerAlias": { "id": 42 } } }
```

`v1.error` — error envelope:
```json
{ "res": "v1.error", "data": { "code": "INVALID_INPUT", "message": "channelId is required" } }
```

Error codes: `INVALID_SOCKET_TOKEN`, `INVALID_JSON`, `INVALID_INPUT`, `UNHANDLED_REQUEST`.

### Subaccount grouping

Channel participation is grouped by subaccount parent: a subaccount participates in a
channel under its server-resolved `parent_account_id`, a root account as itself. A
subaccount and its parent therefore share channel membership and receive each other's
chat; presence stays per-alias so subaccounts still appear as distinct users. Group
resolution is server-side from the ticket-bound alias only (immediate parent, one hop).

Note: `v1.channels.join` / `v1.channels.leave` / `v1.channels.message` request tokens
are Playzoid extensions — upstream Talo drives channel membership over HTTP and only
fans the membership changes out over sockets. The response envelopes stay Talo-verified.

---

## Error Envelope (standardized)

Talo TS uses structured error objects. Use a consistent JSON envelope:
```json
{ "error": { "code": "INVALID_INPUT", "message": "username is required", "details": {"field":"username"} } }
```
Rust helper:
```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorBody {
    pub code: String,
    pub message: String,
    pub details: Option<serde_json::Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorEnvelope {
    pub error: ApiErrorBody,
}
```

---

## Verification actions performed
- Cloned https://github.com/TaloDev/backend and inspected `src/routes` and `src/socket` implementation.
- Extracted socket request/response tokens from `src/socket/messages/socketMessage.ts` and routing validation (socketRouter.ts).
- Extracted Zod schemas used in API routes (login/register, player routes, leaderboard, game-save) and mapped common fields above.
- Phase 0.3 implemented shapes (leaderboards, game saves, WS) verified against `src/entities/leaderboard.rs`, `src/entities/save.rs`, `src/api/leaderboards.rs`, `src/api/saves.rs`, `src/api/socket_ticket.rs`, `src/sockets/{ws,presence,channels}.rs`, and the `migrations/2026082500000{1,2}_*.up.sql` schemas.

## Remaining TODOs (next verification pass)
- [x] Map the remaining complex upstream domain models to full serde structs: `PlayerAlias`, `PlayerAuth`, `GameChannel`, and the full `LeaderboardEntry` (only the public entry *view* is modelled today — upstream also carries `props`, and game-channel / alias models are not persisted server-side yet). → done in task 0.4.2; see "Domain models (upstream parity)" above.
- [x] Upstream routes live under a `/v1` prefix; this server exposes `/auth`, `/players`, `/leaderboards`, `/saves` without it (Phase 0.2 decision) and `/v1/socket-tickets` with it — reconcile on a future parity pass. → done in task 0.4.1: canonical `/v1` mounts now exist with legacy unprefixed aliases.
- [x] Document the Phase 0.4 endpoints (game settings, events, feedback, rate limiting, metrics, OpenAPI). → done in task 0.4.13; `/openapi.json` is the machine-readable source of truth.
- Add HTTP success/error response examples for the remaining upstream (non-implemented) routes as they land.

Verification plan (next steps):
1. Run a script to parse `src/routes/**` for `apiRoute({ ... schema: (z) => ({ body: z.object({...}) }) })` and generate draft Rust structs automatically.
2. Where Zod `.meta()` fields indicate formats, convert to appropriate Rust types (e.g., numbers → i64, date strings → chrono::DateTime).
3. Append generated structs to docs/TALO_API.md and move TODOs to concrete issues in TODO.md.

---

Appendix: sources
- https://github.com/TaloDev/backend (src/routes, src/socket)

---

## Canonical `players` table (database schema)

The initial migration `migrations/20260501000001_create_players.up.sql`
creates the canonical `players` table. This table backs both root
accounts and subaccounts (see `TaloRustServerPlan.md`); subaccounts are
represented by a non-NULL `parent_account_id` referencing another row in
the same table — there is no separate `subaccounts` table.

| Column              | Type                                              | Notes                                                                 |
|---------------------|---------------------------------------------------|-----------------------------------------------------------------------|
| `id`                | `BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT`      | Internal numeric ID. Never exposed in public APIs.                    |
| `public_id`         | `CHAR(36) UNIQUE NOT NULL`                        | UUID v4 used as the external player identifier.                        |
| `username`          | `VARCHAR(64) UNIQUE NOT NULL`                     | Login handle.                                                         |
| `email`             | `VARCHAR(255) UNIQUE NULL`                        | Optional; required for password-reset flows.                          |
| `password_hash`     | `VARCHAR(255) NOT NULL`                           | Argon2id hash (see decision log).                                     |
| `parent_account_id` | `BIGINT UNSIGNED NULL` → `players(id)` (FK)       | NULL = root account. Non-NULL = subaccount of the referenced player.  |
| `status`            | `ENUM('active','suspended','deleted') NOT NULL`   | Default `'active'`. `'deleted'` is a soft-delete tombstone.            |
| `created_at`        | `DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP`     |                                                                       |
| `updated_at`        | `DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP` |                                                       |
| `deleted_at`        | `DATETIME NULL`                                   | Set when `status` transitions to `deleted`.                            |

Indexes:

- `UNIQUE` on `public_id`, `username`, `email`.
- Secondary index on `parent_account_id` for fast subaccount lookup.
- Secondary index on `status` to support active-player queries.

Foreign key behaviour:

- `fk_players_parent_account` uses `ON DELETE SET NULL` so removing a
  parent account does not cascade-destroy its subaccounts; they are
  promoted to root accounts and may then be cleaned up by application
  policy.

The migration pair (`*.up.sql` / `*.down.sql`) is verified in CI by the
optional `sqlx-migrate` job, which runs against an ephemeral MySQL 8
service and exercises both `sqlx migrate run` and `sqlx migrate revert`.

---

## Canonical `leaderboards` and `leaderboard_entries` tables (database schema)

Backing schema for the Phase 0.3 leaderboard endpoints
(`migrations/20260825000001_create_leaderboards.up.sql`).

`leaderboards`:

| Column          | Type                                              | Notes                          |
|-----------------|---------------------------------------------------|--------------------------------|
| `id`            | `BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT`      | Internal. Never exposed.       |
| `internal_name` | `VARCHAR(64) UNIQUE NOT NULL`                     | Route identifier (`{game_id}`).|
| `display_name`  | `VARCHAR(255) NULL`                               |                                |
| `created_at`    | `DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP`     |                                |
| `updated_at`    | `DATETIME ... ON UPDATE CURRENT_TIMESTAMP`        |                                |

`leaderboard_entries`:

| Column          | Type                                              | Notes                                        |
|-----------------|---------------------------------------------------|----------------------------------------------|
| `id`            | `BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT`      | Internal.                                    |
| `leaderboard_id`| `BIGINT UNSIGNED NOT NULL`                        | FK → `leaderboards(id)` ON DELETE CASCADE.   |
| `player_id`     | `BIGINT UNSIGNED NOT NULL`                        | FK → `players(id)` ON DELETE CASCADE.        |
| `score`         | `BIGINT NOT NULL`                                 |                                              |
| `props`         | `JSON NULL`                                       |                                              |
| `created_at`    | `DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP`     |                                              |
| `updated_at`    | `DATETIME ... ON UPDATE CURRENT_TIMESTAMP`        |                                              |

Indexes: `UNIQUE(leaderboard_id, player_id)` (one entry per player per leaderboard),
ranking index `(leaderboard_id, score DESC)` (supports the top-score query), and a
secondary index on `player_id`.

---

## Canonical `game_saves` table (database schema)

Backing schema for the Phase 0.3 game-save endpoints
(`migrations/20260825000002_create_game_saves.up.sql`).

| Column       | Type                                              | Notes                                    |
|--------------|---------------------------------------------------|------------------------------------------|
| `id`         | `BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT`      | Internal. Never exposed.                 |
| `public_id`  | `CHAR(36) UNIQUE NOT NULL`                        | UUID; externally addressed as `id`.      |
| `player_id`  | `BIGINT UNSIGNED NOT NULL`                        | FK → `players(id)` ON DELETE CASCADE.    |
| `name`       | `VARCHAR(255) NOT NULL`                           |                                          |
| `save`       | `JSON NOT NULL`                                   | Arbitrary game-state blob.               |
| `metadata`   | `JSON NULL`                                       | Optional game-specific metadata.         |
| `created_at` | `DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP`     |                                          |
| `updated_at` | `DATETIME ... ON UPDATE CURRENT_TIMESTAMP`        |                                          |

Indexes: `UNIQUE(public_id)`, secondary on `player_id`. Both migration pairs
(`20260825000001_create_leaderboards`, `20260825000002_create_game_saves`) are
reversible (`*.up.sql` / `*.down.sql`) and verified in CI by the optional
`sqlx-migrate` job.

---

## Canonical `game_settings` table (database schema)

Backing schema for the Phase 0.4 game-settings endpoints
(`migrations/20260826000001_create_game_settings.up.sql`). Per-game JSON config,
one row per game.

| Column       | Type                                              | Notes                                    |
|--------------|---------------------------------------------------|------------------------------------------|
| `id`         | `BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT`      | Internal. Never exposed.                 |
| `game_id`    | `VARCHAR(64) UNIQUE NOT NULL`                     | Route identifier (`{game_id}`).          |
| `config`     | `JSON NOT NULL`                                   | Arbitrary per-game settings blob.        |
| `created_at` | `DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP`     | Preserved across updates.                |
| `updated_at` | `DATETIME ... ON UPDATE CURRENT_TIMESTAMP`        |                                          |

Indexes: `UNIQUE(game_id)`.

---

## Canonical `analytics_events` table (database schema)

Backing schema for the Phase 0.4 events + feedback endpoints
(`migrations/20260826000002_create_analytics_events.up.sql`). Append-only event log —
no `updated_at`, no `public_id` (rows are never updated and never addressed by
clients in v0).

| Column       | Type                                              | Notes                                    |
|--------------|---------------------------------------------------|------------------------------------------|
| `id`         | `BIGINT UNSIGNED PRIMARY KEY AUTO_INCREMENT`      | Internal.                                |
| `player_id`  | `BIGINT UNSIGNED NULL`                            | FK → `players(id)` **ON DELETE SET NULL** (deleting a player never erases their events). `NULL` = anonymous. |
| `name`       | `VARCHAR(64) NOT NULL`                            | Event key (`analytics` ingest) or `"feedback"` (feedback submissions). |
| `props`      | `JSON NULL`                                       | Free-form payload.                       |
| `created_at` | `DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP`     | DB-stamped; no client timestamps.        |

Indexes: `(player_id)`, `(name, created_at)` — kept minimal for a high-write log.

<!-- End of file (verified partial mapping). Sonnet produced this update. -->
