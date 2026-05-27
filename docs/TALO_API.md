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

## Playzoid-Server Implemented Endpoints (Phase 0.2)

The sections above document the upstream **Talo TypeScript** API shapes for reference.
The sections below document what is **actually implemented** in this server as of Phase 0.2.

Route prefixes: `/auth`, `/players` (no `/v1` prefix).  
Auth scheme: `Authorization: Bearer <token>` on all protected routes.  
Content-Type: `application/json` for all requests and responses.

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

## HTTP API: leaderboards (/v1/leaderboards)

Verified routes: src/routes/api/leaderboard/* (get, post)

GET /v1/leaderboards/:internalName?query
Response example (Talo-style docs):
```json
{
  "entries": [
    { "playerId": "player1", "score": 1000, "rank": 1 },
    { "playerId": "player2", "score": 950, "rank": 2 }
  ]
}
```

POST /v1/leaderboards/:internalName/entries — body validated by Zod in TS (score + optional props array). See `src/routes/api/leaderboard/post.ts` for exact fields.

Rust example types:
```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitScoreRequest {
    pub player_id: String,
    pub score: i64,
    pub props: Option<Vec<LeaderboardProp>>,
}
```

---

## HTTP API: game saves (/v1/game-saves)

Verified routes: src/routes/api/game-save/*

POST /v1/game-saves
Request (example):
```json
{ "name": "Save name", "playerId": "player1", "save": { /* arbitrary JSON */ }, "metadata": {"zone":"level1"} }
```

Response: save object with id and timestamps. Persist `save` as JSONB/JSON in DB.

Rust:
```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSaveRequest {
    pub name: String,
    pub player_id: String,
    pub save: serde_json::Value,
    pub metadata: Option<serde_json::Value>,
}
```

---

## WebSocket protocol (socket)

Talo uses a typed JSON envelope over a single WebSocket connection. Observed shapes (from `src/socket/messages/socketMessage.ts` and `src/socket/router/socketRouter.ts`):

Client -> Server requests use `req` with a known request token and `data` object:
```json
{ "req": "v1.players.identify", "data": { "playerAliasId": 123, "socketToken": "abc" } }
{ "req": "v1.channels.message", "data": { "channelId": 123, "message": "hi" } }
```

Server -> Client responses use `res` and `data`:
```json
{ "res": "v1.connected", "data": { "now": "..." } }
{ "res": "v1.channels.message", "data": { "channel": { /* channel obj */ }, "message": { /* message obj */ } } }
{ "res": "v1.error", "data": { "code":"INVALID_SOCKET_TOKEN","message":"..." } }
```

The router validates messages against Zod schemas registered in `socket/listeners/*` (e.g. `playerListeners`, `gameChannelListeners`). The routing layer expects `req` to be one of the documented request strings and `res` to be one of the documented response strings.

Rust modeling suggestion:
```rust
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct SocketRequestEnvelope {
    pub req: String,
    pub data: serde_json::Value,
}

#[derive(Serialize)]
pub struct SocketResponseEnvelope<T: Serialize> {
    pub res: String,
    pub data: T,
}
```

For strongly-typed handling, map known `req` strings to enums and `data` to concrete serde structs. Example request types observed:
- v1.players.identify
- v1.channels.message
- v1.player-relationships.broadcast

Observed response types include (non-exhaustive):
- v1.connected
- v1.error
- v1.players.identify.success
- v1.channels.player-joined
- v1.channels.player-left
- v1.channels.message
- v1.channels.deleted
- v1.channels.ownership-transferred
- v1.live-config.updated
- v1.players.presence.updated
- v1.channels.updated
- v1.channels.storage.updated
- v1.player-relationships.broadcast

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

## Remaining TODOs (next verification pass)
- For each API route file under `src/routes/api` extract the exact Zod schema and convert every field to a concrete Rust type in TALO_API.md.
- Map complex domain models: PlayerAlias, PlayerAuth, GameChannel, LeaderboardEntry — produce full serde structs.
- Add examples of HTTP success and error responses for each validated route.

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

<!-- End of file (verified partial mapping). Sonnet produced this update. -->
