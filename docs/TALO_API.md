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

<!-- End of file (verified partial mapping). Sonnet produced this update. -->
