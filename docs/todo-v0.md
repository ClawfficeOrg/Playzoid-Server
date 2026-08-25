# Playzoid-Server Todo — v0.x: Foundation, Auth & Talo Parity

> Back to index: [`todo.md`](todo.md)
>
> v0 covers everything up to the first stable release: scaffolding,
> authentication + player management, leaderboards / game saves / WebSocket
> channels, then production hardening.

## How to run this file (ralph)

- One task per loop. Read `AGENTS.md`, `docs/memory.md`, and
  `docs/learnings.md` before starting.
- Each task lists **Owned paths** where relevant — touch only those; flag
  deviations in the done-note.
- Green gate per task: `cargo fmt --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo test`. Integration tests marked
  `#[ignore]` additionally require the Docker dev stack; run them with
  `cargo test --test '*' -- --ignored`.
- All work goes through PRs against `main`; never push directly to `main`.

## Versioning conventions (semver)

- Task ids are full semver `X.Y.Z`: **Y = release line (phase), Z = task
  number within the line.**
- One release line maps to one `release/vX.Y` branch; each task gets a
  `task/vX.Y.Z` branch off it.
- On line completion: review → merge to main → tag `vX.Y.0` → bump project
  version to exactly `X.Y.0`.
- Lines where Y = 0 (e.g. `1.0`) are major-adjacent and require an RC tag +
  human sign-off before merge.

---

## Phase 0.1 — Foundation & Project Scaffolding ✅ (merged via PRs #4–#9)

- [x] `0.1.1` Initialise `Cargo.toml` with planned dependencies
- [x] `0.1.2` Project folder layout (`src/api`, entities, middleware, services, sockets)
- [x] `0.1.3` `main.rs` server startup + config from `.env`
- [x] `0.1.4` `/healthz` endpoint + WS ping
- [x] `0.1.5` sqlx pool with MySQL support
- [x] `0.1.6` Initial migration: `players` table
- [x] `0.1.7` tracing + structured logging
- [x] `0.1.8` `.env.example` + `config/docker-compose.dev.yml`
- [x] `0.1.9` Multi-stage Dockerfile
- [x] `0.1.10` GitHub Actions CI (fmt + clippy + test + audit + docker smoke + sqlx-migrate)
- [x] `0.1.11` `docs/CHANGES.md` initial entry

Status restored after the OpenClaw blow-up — see `docs/RECOVERY_PLAN.md`
(R-3..R-10). All gaps closed.

## Phase 0.2 — Authentication & Player Management ✅ (PRs #11–#14 merged)

- [x] `0.2.1` `POST /auth/login` — validate credentials, issue JWT
- [x] `0.2.2` JWT Bearer auth middleware extractor (`AuthenticatedUser`)
- [x] `0.2.3` `POST /auth/register` — create player account
- [x] `0.2.4` `GET /players/{id}` with auth guard
- [x] `0.2.5` `PUT /players/{id}` — update profile
- [x] `0.2.6` `DELETE /players/{id}` — soft delete
- [x] `0.2.7` `parent_account_id` schema verification (already present from R-8)
- [x] `0.2.8` `POST /players/subaccount`
- [x] `0.2.9` `GET /players/{id}/subaccounts`
- [x] `0.2.10` Redis session caching with TTL
- [x] `0.2.11` Unit tests for auth service
- [x] `0.2.12` Integration tests for `/auth` and `/players`
- [x] `0.2.13` Document `/auth` and `/players` in `docs/TALO_API.md`

## Phase 0.3 — Leaderboards, Game Saves & WebSocket Channels 🔄

Success criteria: leaderboard GET/POST/PUT work with auth; game saves CRUD;
WebSocket `/ws` broadcasts presence/chat; load test passes 100 concurrent WS
connections.

- [x] `0.3.1` DB migration: `leaderboards` + `leaderboard_entries` tables.
  Complexity: Small. Owned paths: `migrations/`. ✅ Done (PR #15).
  Reversible up/down pair; unique `(leaderboard_id, player_id)`; ranking
  index `(leaderboard_id, score DESC)`.
- [x] `0.3.2` Implement `GET /leaderboards/{game_id}` — paginated top scores.
  Complexity: Medium. Owned paths: `src/api/leaderboards.rs`,
  `src/services/leaderboards.rs`, `src/entities/leaderboard.rs`. ✅ Done
  (PR #16). `page`/`per_page` pagination, ranks continue across pages,
  camelCase Talo shape.
- [x] `0.3.3` Implement `POST /leaderboards/{game_id}/entries` — submit score.
  Complexity: Small. Owned paths: same as 0.3.2. ✅ Done (PR #17). Player
  identity from JWT; duplicate → 409; props JSON ≤ 4KB; returns computed rank.
- [x] `0.3.4` Implement `PUT /leaderboards/{game_id}/entries/{player_id}` —
  update score. Complexity: Small. Owned paths: same as 0.3.2. ✅ Done
  (PR #18). Own-entry only (403 cross-player); omitted props preserved.
- [x] `0.3.5` DB migration: `game_saves` table.
  Complexity: Small. Owned paths: `migrations/`.
  Agent: basic_dev_agent
  <!-- 0.3.5 done note: migrations/20260825000002_create_game_saves.{up,down}.sql; verified up/down against dev stack -->
- [ ] `0.3.6` Implement `GET /saves/{player_id}` — list saves.
  Complexity: Small. Owned paths: `src/api/saves.rs` (new),
  `src/services/saves.rs` (new), `src/entities/save.rs` (new).
  Agent: basic_dev_agent
- [ ] `0.3.7` Implement `POST /saves` — create game save (JSON blob).
  Complexity: Medium. Owned paths: same as 0.3.6.
  Agent: mid_dev_agent
- [ ] `0.3.8` Implement `GET /saves/{player_id}/{save_id}` — retrieve save.
  Complexity: Small. Owned paths: same as 0.3.6.
  Agent: basic_dev_agent
  <!-- 0.3.5 done note: migrations/20260825000002_create_game_saves.{up,down}.sql; verified up/down against dev stack -->
- [ ] `0.3.9` Implement `DELETE /saves/{player_id}/{save_id}`.
  Complexity: Small. Owned paths: same as 0.3.6.
  Agent: basic_dev_agent
  <!-- 0.3.5 done note: migrations/20260825000002_create_game_saves.{up,down}.sql; verified up/down against dev stack -->
- [ ] `0.3.10` Implement WebSocket `/ws` handler with actix-web WS upgrade.
  Complexity: High. Owned paths: `src/sockets/ws.rs`.
  Agent: pro_dev_agent
- [ ] `0.3.11` WebSocket: player connect/disconnect presence broadcast.
  Complexity: Medium. Owned paths: `src/sockets/`.
  Agent: mid_dev_agent
- [ ] `0.3.12` WebSocket: channel join/leave message types.
  Complexity: Medium. Owned paths: `src/sockets/`.
  Agent: mid_dev_agent
- [ ] `0.3.13` WebSocket: chat message broadcast within channel.
  Complexity: Medium. Owned paths: `src/sockets/`.
  Agent: mid_dev_agent
- [ ] `0.3.14` WebSocket: subaccount participant support (parent_account_id grouping).
  Complexity: Medium. Owned paths: `src/sockets/`.
  Agent: mid_dev_agent
- [ ] `0.3.15` Write WS load test (100 concurrent connections, message throughput).
  Complexity: High. Owned paths: `tests/` or `bench/`.
  Agent: pro_dev_agent
- [ ] `0.3.16` Write unit + integration tests for leaderboard and save endpoints.
  Complexity: Medium. Owned paths: `tests/`.
  Agent: mid_dev_agent
- [ ] `0.3.17` Update `docs/TALO_API.md` with leaderboard, save, WS shapes.
  Complexity: Small. Owned paths: `docs/TALO_API.md`.
  Agent: basic_dev_agent
  <!-- 0.3.5 done note: migrations/20260825000002_create_game_saves.{up,down}.sql; verified up/down against dev stack -->

### Milestone review steps (Phase 0.3)

1. All cargo tests pass
2. WS load test: 100 connections, 1000 messages — no drops
3. Manual: submit score → GET leaderboard shows it ranked
4. Create save → retrieve save → JSON matches

---

<!-- New phases for v0 append below. v1+ goes in docs/todo-v1.md. -->
