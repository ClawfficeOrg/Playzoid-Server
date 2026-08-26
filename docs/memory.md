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

<!-- Append new decisions below this line. Use the dated heading format above. -->

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
