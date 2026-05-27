# Agent Instructions

These instructions apply to every agent working in this repository.

---

## Required Reading

Before starting any task, read:

- [`docs/GUIDELINES.md`](docs/GUIDELINES.md) — code standards, CI, branching and PR rules
- [`docs/TODO.md`](docs/TODO.md) — phased task list with per-task status tracking
- [`docs/RECOVERY_PLAN.md`](docs/RECOVERY_PLAN.md) — recovery context and per-task loop contract
- [`memory/projects/playzoid-server/project.md`](memory/projects/playzoid-server/project.md) — architectural decision log

Also read as relevant to your specific task:

- [`docs/TALO_API.md`](docs/TALO_API.md) — verified Talo API surface shapes
- [`docs/TALO_API_STRUCTS.md`](docs/TALO_API_STRUCTS.md) — raw Rust struct extractions (note the `RAW EXTRACTION ARTIFACT` header; rename structs before lifting)
- [`docs/TaloRustServerPlan.md`](docs/TaloRustServerPlan.md) — full server architecture plan

---

## Branching Model

All work goes through PRs against `main`. Never push directly to `main`.

| Branch type     | Pattern                                          |
|-----------------|--------------------------------------------------|
| Feature         | `feature/<scope>/<short-description>`            |
| Fix             | `fix/<scope>/<short-description>`                |
| Refactor        | `refactor/<scope>/<description>`                 |
| Docs            | `docs/<description>`                             |
| Chore / tooling | `chore/<description>`                            |
| Ralph tasks     | `task/<phase>-<num>` (e.g. `task/0.2-1`)         |

**Never self-approve or auto-merge PRs you authored.** All PRs require CI green
plus at least one human reviewer before merge.

---

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <short description>

[optional body]
[optional footer(s): e.g. "Closes task 0.2-1"]
```

Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `ci`, `perf`

---

## Code Quality

Rules:

- Use `Result<T, E>` consistently; no `unwrap()` or `expect()` outside of tests.
- Use `thiserror` for defining error types; `anyhow` is acceptable in tests and scripts.
- Use `tracing::instrument` on handler functions for automatic span creation.
- All public structs and functions must have doc comments (`///`).
- Database query results must use `sqlx::FromRow` — avoid raw tuple mapping.
- Validate all request bodies before processing.
- Use parameterized `sqlx` queries — never string interpolation in SQL.

Security requirements:

- **Never hardcode secrets.** All credentials go in `.env` (local) or environment variables (CI/prod).
- Passwords must be hashed with **Argon2id** (`argon2` crate) — never stored plain.
- JWT secret sourced from env, validated ≥ 32 bytes at startup.
- Validate and sanitise all user-supplied input (paths, IDs, identifiers).
- Run `cargo audit` periodically; flag new advisories in `docs/ralph-log.md`.

---

## Testing

Baseline checks (run these before committing — there is no pre-commit hook
that runs them automatically):

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

All three must pass before opening a PR.

Every implementation task must also:

- Include unit tests in the same file (`#[cfg(test)] mod tests {}`).
- Include integration tests in `tests/` for any new HTTP endpoint.

---

## PR Requirements

Every PR description (whether opened by ralph or by a human) must include:

- `Closes task <id>` (e.g. `Closes task 0.2-1`)
- The acceptance criteria from `docs/TODO.md` with each item checked
- `cargo test` output (last 20 lines)
- For WS or load-sensitive changes: load test results

---

## Documentation and Memory

After completing any task:

- Update `docs/CHANGES.md` with a brief changelog entry.
- Append a decision entry to `memory/projects/playzoid-server/project.md` if
  an architectural choice was made.
- Update the task status in `docs/TODO.md` (see format below).

### TODO.md Status Markers

| Marker             | Meaning                                        |
|--------------------|------------------------------------------------|
| `⏳ pending`       | Not yet started — ralph will pick this up      |
| `🔄 in-progress`  | Branch created; work in flight                 |
| `📬 PR #<n>`       | Branch pushed; PR open; awaiting human merge   |
| `🟡 partial`       | Partially complete; more work needed           |
| `✅ done`          | Complete and merged                            |
| `✅ verified`      | Pre-existing; verified correct during recovery |

---

## Skills

If a relevant skill exists in `skills/`, use it. Skills keep work consistent.

- `skills/ralph.md` — autonomous task loop agent skill
