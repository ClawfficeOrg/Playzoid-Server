# Playzoid-Server — Project Decision Log

> Long-term decision record for the Playzoid-Server crate. New decisions are
> appended at the bottom with a date stamp, the alternatives considered, and
> the reasoning that won. Do not rewrite history — supersede entries with new
> ones if the decision changes.

This file is referenced by [`docs/GUIDELINES.md`](../../../docs/GUIDELINES.md)
and [`docs/TODO.md`](../../../docs/TODO.md).

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

<!-- Append new decisions below this line. Use the dated heading format above. -->

---

## Open Questions / Assumptions

These mirror the open questions in [`docs/TODO.md`](../../../docs/TODO.md). Resolve
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
