# Playzoid-Server Todo — index

Master index of the versioned todo files. Each `todo-vN.md` is a
self-contained, ralph-driven plan for one major version; read `AGENTS.md`,
`docs/memory.md`, and `docs/learnings.md` before starting any task.
Ralph reads task lines (`- [ ] \`X.Y.Z\``) from the versioned files via
`all_todo_lines()` — do not add task lines directly here.

## Versioning conventions (semver)

- Task ids are full semver `X.Y.Z`: **Y = release line, Z = task number.**
- Each release line gets a `release/vX.Y` branch; each task a `task/vX.Y.Z`
  branch off it.
- On line completion: review → merge to main → tag `vX.Y.0` → bump project
  version to exactly `X.Y.0`.
- Lines where Y = 0 require an RC tag + human sign-off before merge.

## Current

- [`todo-v0.md`](todo-v0.md) — **v0.x**: foundation, auth & player management,
  leaderboards / game saves / WebSocket channels.
- [`todo-v1.md`](todo-v1.md) — **v0.x line 0.4 active** ⭐: analytics, config,
  feedback & production hardening (Talo parity, rate limiting, Prometheus
  metrics, OpenAPI).

## Conventions

- Tasks: `- [ ] \`X.Y.Z\`` — checked off on merge
- Phase merge: `- [ ] release/vX.Y → main` — checked after review
- Major versions require human sign-off before merge to main

## History

- Phase 0.1 (scaffolding) ✅ — restored after recovery, see `docs/RECOVERY_PLAN.md`
- Phase 0.2 (auth + players) ✅ — PRs #11–#14
- Phase 0.3 progress is tracked inline in `todo-v0.md`

## Related docs

- `AGENTS.md`, `docs/memory.md`, `docs/learnings.md` — required reading per task
- `docs/TALO_API.md` — verified upstream API surface
- `docs/GUIDELINES.md` — code standards, CI, branching rules
