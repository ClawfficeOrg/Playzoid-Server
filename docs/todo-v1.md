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

- [ ] `0.4.3` DB migration: `game_settings` table (per-game JSON config,
  reversible up/down pair).
  Complexity: Small. Owned paths: `migrations/`.
  Agent: basic_dev_agent
- [ ] `0.4.4` Implement `GET /v1/games/{game_id}/settings` and
  `PUT .../settings` (auth-guarded, validated JSON ≤ size cap).
  Complexity: Medium. Owned paths: `src/api/game_settings.rs` (new),
  `src/services/game_settings.rs` (new), `src/entities/game_setting.rs` (new).
  Agent: mid_dev_agent

### Analytics & feedback

- [ ] `0.4.5` DB migration: `analytics_events` table (append-only event log).
  Complexity: Small. Owned paths: `migrations/`.
  Agent: basic_dev_agent
- [ ] `0.4.6` Implement `POST /v1/events` — ingest analytics events
  (batched array body, validate shape, fire-and-forget semantics).
  Complexity: Medium. Owned paths: `src/api/events.rs` (new),
  `src/services/events.rs` (new), `src/entities/analytics_event.rs` (new).
  Agent: mid_dev_agent
- [ ] `0.4.7` Implement `POST /v1/feedback` — player feedback submission.
  Complexity: Small. Owned paths: `src/api/feedback.rs` (new),
  `src/services/feedback.rs` (new).
  Agent: basic_dev_agent

### Production hardening

- [ ] `0.4.8` Rate limiting middleware on public routes (Redis-backed token
  bucket or fixed window; configurable limits via env).
  Complexity: High. Owned paths: `src/middleware/`, `src/config.rs`.
  Agent: pro_dev_agent
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
