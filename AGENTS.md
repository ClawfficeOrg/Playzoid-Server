# Agent Instructions

These instructions apply to every agent working in this repository.

---

## Required Reading

Before starting any task, read:

- [`docs/GUIDELINES.md`](docs/GUIDELINES.md) — code standards, CI, branching and PR rules
- [`docs/todo.md`](docs/todo.md) — master todo index (versioned todo files live alongside it)
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
| Ralph tasks     | `task/vX.Y.Z` (e.g. `task/v0.3.5`)               |

**Never self-approve or auto-merge PRs you authored.** All PRs require CI green
plus at least one human reviewer before merge.

---

## Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <short description>

[optional body]
[optional footer(s): e.g. "Closes task 0.3.5"]
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

- `Closes task <id>` (e.g. `Closes task 0.3.5`)
- The acceptance criteria from the task block in `docs/todo-v<N>.md` with each item checked
- `cargo test` output (last 20 lines)
- For WS or load-sensitive changes: load test results

---

## Documentation and Memory

After completing any task:

- Update `docs/CHANGES.md` with a brief changelog entry.
- Append a decision entry to `docs/memory.md` if an architectural choice was
  made, and a learning to `docs/learnings.md` if there is a durable lesson.
- Mark the task `- [x]` in its `docs/todo-v<N>.md` file.

### Todo conventions

Tasks use semver ids `X.Y.Z` (`Y` = release line, `Z` = task number):

```
- [ ] `0.3.5` Task title
  Complexity: Small
  Agent: basic_dev_agent
```

- `[ ]` open — ralph will pick it up; `[x]` done and merged.
- One release line per `release/vX.Y` branch; one `task/vX.Y.Z` branch per task.
- On line completion: review → merge → tag `vX.Y.0` → bump project version.
- Lines where Y = 0 need an RC tag + human sign-off before merge.

---

## Skills

If a relevant skill exists in `skills/`, use it. Skills keep work consistent.

- `skills/ralph.md` — autonomous task loop agent skill
