# Playzoid-Server Todo — v0.x continued: Analytics, Config, Feedback & Production Hardening

> Back to index: [`todo.md`](todo.md)
>
> Continues the v0.x line after Phase 0.3 (merged, tagged `v0.3.0`). Covers
> the remaining Talo API surface (game config/settings, analytics events,
> feedback), the `/v1` route-prefix parity pass, and production hardening
> (rate limiting, Prometheus metrics, OpenAPI).

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
- Lines where Y = 0 (e.g. this line) are major-adjacent and require an RC tag
  + human sign-off before merge.

---

## Phase 0.4 — Analytics, Config, Feedback & Production Hardening 🔄

Success criteria: game settings readable/writable per game; analytics events
accepted and stored; feedback endpoint live; rate limiting active on public
routes; `/metrics` scraped by Prometheus; OpenAPI spec served and accurate;
full Talo API parity documented in `docs/TALO_API.md`.

### Parity & data model

- [x] `0.4.1` Reconcile route prefixes — move `/auth`, `/players`,
  `/leaderboards`, `/saves` under `/v1` (upstream parity), keep old paths as
  aliases during transition.
  Complexity: Medium. Owned paths: `src/api/`, `tests/`.
  Agent: mid_dev_agent
- [x] `0.4.2` Full domain-model structs: `PlayerAlias`, `PlayerAuth`,
  `GameChannel`, complete `LeaderboardEntry` (incl. upstream `props`).
  Complexity: Medium. Owned paths: `src/entities/`, `docs/TALO_API.md`.
  Agent: mid_dev_agent

### Config / game settings

- [x] `0.4.3` DB migration: `game_settings` table (per-game JSON config,
  reversible up/down pair).
  Complexity: Small. Owned paths: `migrations/`.
  Agent: basic_dev_agent
  <!-- 0.4.3 done note: migrations/20260826000001_create_game_settings.{up,down}.sql; reversibility verified locally with sqlx-cli against a throwaway mysql:8.0 schema (full up set applies, ours sorts last lexicographically; one revert drops exactly game_settings). CI sqlx-migrate job runs the same sequence but is NOT a gate: workflow is workflow_dispatch-only and the job is continue-on-error:true -->
- [x] `0.4.4` Implement `GET /v1/games/{game_id}/settings` and
  `PUT .../settings` (auth-guarded, validated JSON ≤ size cap).
  Complexity: Medium. Owned paths: `src/api/game_settings.rs` (new),
  `src/services/game_settings.rs` (new), `src/entities/game_setting.rs` (new).
  Agent: mid_dev_agent
  <!-- 0.4.4 done note: implemented 2026-08-26; canonical-only /v1/games scope (no legacy alias, post-0.4.1 route); JWT guard via AuthenticatedUser; pre-SQL validation (game_id 1..=64 chars, config non-null, ≤32KiB MAX_CONFIG_BYTES mirroring saves); parameterized upsert (ON DUPLICATE KEY UPDATE config) returning read-back GameSettingView, created_at preserved; read-back trims game_id symmetrically with PUT so GET addresses the exact row a PUT created; upstream has no per-game settings endpoint — documented Playzoid extension in memory.md incl. any-JWT-writes trade-off until games/scopes exist; unit tests same-file (15) + tests/game_settings_integration.rs (10, #[ignore], Docker stack — note: tests/ nominally 0.4.12's gap-fill owner, per-endpoint suite precedent from 0.3.x); cargo fmt/clippy/test green; commit + PR deferred per session instructions -->

### Analytics & feedback

- [x] `0.4.5` DB migration: `analytics_events` table (append-only event log).
  Complexity: Small. Owned paths: `migrations/`.
  Agent: basic_dev_agent
  <!-- 0.4.5 done note: migrations/20260826000002_create_analytics_events.{up,down}.sql; append-only by schema (no updated_at/public_id), nullable player_id FK ON DELETE SET NULL (player deletion must never erase event history), generic name+JSON props (typed schema deferred to Phase 1.0; upstream shape undocumented in repo — no columns guessed); minimal indexes (player_id, name+created_at) for high-write log; reversibility verified locally with sqlx-cli against a throwaway mysql:8.0 schema per the 0.4.3 pattern; commit + PR deferred per session instructions -->
- [x] `0.4.6` Implement `POST /v1/events` — ingest analytics events
  (batched array body, validate shape, fire-and-forget semantics).
  Complexity: Medium. Owned paths: `src/api/events.rs` (new),
  `src/services/events.rs` (new), `src/entities/analytics_event.rs` (new).
  Agent: mid_dev_agent
  <!-- 0.4.6 done note: implemented 2026-08-26; canonical-only /v1/events route (no legacy alias, post-0.4.1 precedent); bare JSON array body with deny_unknown_fields per event and no client timestamps (created_at DB-stamped); whole-batch pre-SQL validation (non-empty, ≤100 MAX_BATCH_EVENTS, trimmed name 1..=64 matching VARCHAR(64), serialized props ≤4KiB MAX_PROPS_BYTES); fire-and-forget contract: 202 {"accepted":n} after one batched multi-row INSERT, post-validation DB failures logged+swallowed at API layer (unit-tested via dead pool), pool absent → 503; best-effort attribution (unknown/deleted caller or resolution failure → player_id NULL rows, never 404); INSERT built with sqlx::QueryBuilder::push_values — placeholder scaffolding + push_bind only, no interpolated user data (explicit SQL-safety comment in code); unit tests same-file (API 11 + service 7 + entity 2) + tests/events_integration.rs (10 #[ignore], Docker stack — tests/ nominally 0.4.12's owner, per-endpoint suite precedent from 0.3.x/0.4.4); wired into mods + main.rs (main.rs one-line configure outside owned paths, same accepted deviation as 0.4.4); cargo fmt/clippy/test green; commit + PR deferred per session instructions -->
- [x] `0.4.7` Implement `POST /v1/feedback` — player feedback submission.
  Complexity: Small. Owned paths: `src/api/feedback.rs` (new),
  `src/services/feedback.rs` (new).
  Agent: basic_dev_agent
  <!-- 0.4.7 done note: implemented 2026-08-26; canonical-only /v1/feedback route (no legacy alias, post-0.4.1 precedent); body {"message"} with deny_unknown_fields, pre-SQL validation (trimmed 1..=1000 chars MAX_MESSAGE_CHARS mirroring the 0.3.13 chat gate; JSON-encoded props ≤4KiB MAX_PROPS_BYTES mirroring events — escape-heavy input trips the encoded cap while length-valid); stored as name="feedback" rows in the existing append-only analytics_events table (owned paths exclude migrations/entities — sink-reuse decision recorded in memory.md, dedicated table = Phase 1.0 candidate); deliberate divergence from fire-and-forget events: post-validation DB failure → honest 500 {"error":"internal error"} (user content must not be silently dropped), details logged server-side only; best-effort attribution (unknown/deleted caller or failed resolution → player_id NULL, never an error); static .bind()-only INSERT; unit tests same-file (API 10 + service 7) + tests/feedback_integration.rs (5 #[ignore], Docker stack — tests/ nominally 0.4.12's owner, per-endpoint suite precedent from 0.3.x/0.4.4/0.4.6); wired into mods + main.rs (one-line configure outside owned paths, same accepted deviation as 0.4.4/0.4.6); cargo fmt/clippy/test green (215 passed); commit + PR deferred per session instructions -->
### Production hardening

- [x] `0.4.8` Rate limiting middleware on public routes (Redis-backed token
  bucket or fixed window; configurable limits via env).
  Complexity: High. Owned paths: `src/middleware/`, `src/config.rs`.
  Agent: pro_dev_agent
  <!-- 0.4.8 done note: implemented 2026-08-26; fixed-window Redis limiter, one key per (class, ip, window_start) via atomic EVAL INCR+EXPIRE+TTL; classes auth (credential prefixes, tight budget) + default (public prefixes); fail-open degraded mode (Redis down at boot → no app_data → pass-through; mid-flight error → allow + warn); X-Forwarded-For opt-in only; 429 = req.into_response() consuming ServiceRequest directly — never clone the inner HttpRequest, actix panics on match_info_mut() when the downstream Rc has refcount > 1; X-RateLimit-* + Retry-After headers, JSON error body; middleware future is 'static so inner service is wrapped in Arc<S> (S::Future: 'static) — &self cannot be captured past call(); RateLimiter monomorphic over Arc<dyn WindowCounter> (object-safe 'static CounterFuture) so app_data type matches; injectable NowFn clock for window-rollover tests; unit tests same-file (24 incl. deplete-then-429, 429 holds until Retry-After-1, resets after elapse) + tests/ rate-limit integration deferred to 0.4.12; wired in main.rs (wrap + conditional app_data); cargo fmt/clippy/test green (239 passed) -->
- [ ] `0.4.9` Prometheus `/metrics` endpoint (request count/latency histograms,
  WS connection gauge, DB pool stats).
  Complexity: Medium. Owned paths: `src/middleware/`, `src/api/metrics.rs` (new),
  `Cargo.toml`.
  Agent: mid_dev_agent
- [ ] `0.4.10` Serve OpenAPI document (`/openapi.json`) generated from route
  definitions; keep in CI to fail on drift.
  Complexity: High. Owned paths: `src/api/`, `Cargo.toml`, `.github/workflows/`.
  Agent: pro_dev_agent
- [ ] `0.4.11` Tighten CI: make `cargo audit` gating (`continue-on-error:
  false`) now that all top-level deps are pinned (RECOVERY_PLAN R-follow-up).
  Complexity: Small. Owned paths: `.github/workflows/`.
  Agent: basic_dev_agent

### Docs & tests

- [ ] `0.4.12` Unit + integration tests for all new endpoints (settings,
  events, feedback) incl. rate-limit behaviour.
  Complexity: Medium. Owned paths: `tests/`.
  Agent: mid_dev_agent
- [ ] `0.4.13` Update `docs/TALO_API.md`: new endpoint shapes, `/v1` prefix,
  remaining-TODOs trim.
  Complexity: Small. Owned paths: `docs/TALO_API.md`.
  Agent: basic_dev_agent

### Milestone review steps (Phase 0.4)

1. All cargo tests pass (unit + ignored integration vs Docker stack)
2. Rate limit returns 429 after configured burst; recovers after window
3. Prometheus scrape shows request metrics and WS gauges
4. `/openapi.json` validates and matches implemented routes
5. Manual: PUT settings → GET settings round-trips JSON

---

<!-- New phases for v0.x append below. -->
