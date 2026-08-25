#!/bin/bash
# ralph — autonomous task loop (generic baseline, semver task ids).
#
# Adapted for this repository. The language-agnostic parts live in the
# CONFIG block below; per-repo specialisation is done via env overrides or
# by editing that block only. Everything else is generic.
#
# Usage:
#   ./scripts/ralph.sh                          # multi-phase: loop through all open release lines
#   ./scripts/ralph.sh 0.3.5                    # single task (infers release branch)
#   ./scripts/ralph.sh --minutes=30             # loop for up to 30 minutes
#   ./scripts/ralph.sh --hours=2                # loop for up to 2 hours
#   ./scripts/ralph.sh --hours=1 --minutes=30   # combined time limit
#   ./scripts/ralph.sh --until=0.3.9            # run all open tasks up to and including a task id
#   ./scripts/ralph.sh --dry-run                # preview the next action and exit
#   ./scripts/ralph.sh --log=/tmp/run.log       # tee everything to a file
#   ./scripts/ralph.sh --quiet                  # suppress agent stderr
#
# Versioning (semver):
#   Task ids are X.Y.Z — Y = release line, Z = task number within the line.
#   Each line gets a `release/vX.Y` branch; each task a `task/vX.Y.Z` branch.
#   On line completion: review → merge to main → tag `vX.Y.0` → bump project
#   version to exactly `X.Y.0`. Lines with Y = 0 are gated behind an RC tag +
#   human sign-off (ralph never auto-merges them).
#
# Ralph can be started from:
#   main            — picks up the next open release line and loops through all lines.
#   release/vX.Y    — resumes that line, then continues through subsequent lines.
#
# Stopping gracefully (finishes the current task first):
#   touch scripts/STOP.md               # sentinel file in the repo
#   kill -TERM $(cat /tmp/ralph.pid)    # SIGTERM to the process
#   Ctrl-C                              # SIGINT
#
# Requires: an agent CLI (see AGENT block), git, a clean working tree.

set -e

REPO_ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# ══════════════════════════════════════════════════════════════════════════
# CONFIG — per-repo adaptation happens HERE only.
# ══════════════════════════════════════════════════════════════════════════

REPO_NAME="$(basename "$REPO_ROOT")"
SKILL="${SKILL:-$REPO_ROOT/skills/ralph.md}"
LOG="$REPO_ROOT/docs/ralph-log.md"
TODO_GLOB='todo-v*.md'          # glob of versioned todo files under docs/
DEFAULT_BRANCH="${DEFAULT_BRANCH:-main}"

# Check commands (green gate). Override via env: FMT_CMD=... ralph.sh
detect_checks() {
    if [ -f Cargo.toml ]; then
        FMT_CMD="${FMT_CMD:-cargo fmt --check}"
        LINT_CMD="${LINT_CMD:-cargo clippy --all-targets --all-features -- -D warnings}"
        TEST_CMD="${TEST_CMD:-cargo test}"
        LANG_NAME="Rust"
    elif [ -f package.json ]; then
        FMT_CMD="${FMT_CMD:-npx prettier --check .}"
        LINT_CMD="${LINT_CMD:-npm run lint}"
        TEST_CMD="${TEST_CMD:-npm test}"
        LANG_NAME="TypeScript/JavaScript"
    elif [ -f pyproject.toml ] || [ -f setup.py ]; then
        FMT_CMD="${FMT_CMD:-ruff format --check .}"
        LINT_CMD="${LINT_CMD:-ruff check .}"
        TEST_CMD="${TEST_CMD:-pytest}"
        LANG_NAME="Python"
    elif [ -f go.mod ]; then
        FMT_CMD="${FMT_CMD:-gofmt -l .}"
        LINT_CMD="${LINT_CMD:-go vet ./...}"
        TEST_CMD="${TEST_CMD:-go test ./...}"
        LANG_NAME="Go"
    else
        FMT_CMD="${FMT_CMD:-}"
        LINT_CMD="${LINT_CMD:-}"
        TEST_CMD="${TEST_CMD:-}"
        LANG_NAME="unknown"
    fi
}
detect_checks

# Project version bump target. First match wins; empty → skip version bump.
# Supported: Cargo.toml ([package] version), package.json ("version"),
# pyproject.toml (project.version), VERSION plain file.
version_bump() {
    _ver="$1"   # e.g. 0.3.0
    if [ -f Cargo.toml ] && grep -q '^version = ' Cargo.toml; then
        if sed --version >/dev/null 2>&1; then
            sed -i "s/^version = \".*\"/version = \"${_ver}\"/" Cargo.toml
        else
            sed -i '' "s/^version = \".*\"/version = \"${_ver}\"/" Cargo.toml
        fi
        git add Cargo.toml
    elif [ -f package.json ] && command -v node >/dev/null 2>&1; then
        node -e "const f='package.json',j=require('./'+f);j.version='${_ver}';require('fs').writeFileSync(f,JSON.stringify(j,null,2)+'\n')"
        git add package.json
    elif [ -f pyproject.toml ]; then
        if sed --version >/dev/null 2>&1; then
            sed -i "s/^version = \".*\"/version = \"${_ver}\"/" pyproject.toml
        else
            sed -i '' "s/^version = \".*\"/version = \"${_ver}\"/" pyproject.toml
        fi
        git add pyproject.toml
    elif [ -f VERSION ]; then
        printf '%s\n' "$_ver" > VERSION
        git add VERSION
    else
        return 1
    fi
}

# ══════════════════════════════════════════════════════════════════════════
# AGENT configuration — each agent is a PROVIDER/MODEL pair.
# Providers: opencode-go → opencode, github-copilot → copilot,
#            claude-code → claude, kilocode → kilo
# ══════════════════════════════════════════════════════════════════════════

TASK_PLANNING_AGENT="${TASK_PLANNING_AGENT:-opencode-go/deepseek-v4-flash}"
BASIC_DEV_AGENT="${BASIC_DEV_AGENT:-opencode-go/deepseek-v4-flash}"
MID_DEV_AGENT="${MID_DEV_AGENT:-opencode-go/deepseek-v4-flash}"
PRO_DEV_AGENT="${PRO_DEV_AGENT:-github-copilot/claude-sonnet-4.6}"
TASK_REVIEW_AGENT="${TASK_REVIEW_AGENT:-opencode-go/deepseek-v4-flash}"
RELEASE_REVIEW_AGENT="${RELEASE_REVIEW_AGENT:-github-copilot/claude-sonnet-4.6}"
MAJOR_RELEASE_REVIEW_AGENT="${MAJOR_RELEASE_REVIEW_AGENT:-github-copilot/claude-opus-4.8}"
ARCHITECT_AGENT="${ARCHITECT_AGENT:-github-copilot/claude-sonnet-4.6}"

# Caveman mode: prepend compressed-output instructions to every prompt.
CAVEMAN="${CAVEMAN:-0}"
CAVEMAN_LEVEL="${CAVEMAN_LEVEL:-full}"

# Merge policy:
#   local — ralph commits/merges locally into release/vX.Y and auto-merges
#           completed lines to main after a release review (Zoid-style).
#   pr    — ralph pushes a task branch and opens a PR, then stops; a human
#           reviews and merges everything (safe default for strict repos).
MERGE_MODE="${MERGE_MODE:-local}"

BASE_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
case "$BASE_BRANCH" in master) BASE_BRANCH="$DEFAULT_BRANCH" ;; esac

# ── colours / logging ────────────────────────────────────────────────────────
BOLD=$(tput bold 2>/dev/null || true)
CYAN=$(tput setaf 6 2>/dev/null || true)
GREEN=$(tput setaf 2 2>/dev/null || true)
YELLOW=$(tput setaf 3 2>/dev/null || true)
RED=$(tput setaf 1 2>/dev/null || true)
RESET=$(tput sgr0 2>/dev/null || true)

log()  { printf '%s\n' "${CYAN}[ralph]${RESET} $*"; }
good() { printf '%s\n' "${GREEN}[ralph]${RESET} $*"; }
warn() { printf '%s\n' "${YELLOW}[ralph]${RESET} $*"; }
die()  { printf '%s\n' "${RED}[ralph]${RESET} $*" >&2; exit 1; }

# ── argument parsing ─────────────────────────────────────────────────────────
SINGLE_TASK=""
UNTIL_TASK=""
DURATION_SECS=0
DRY_RUN=0
QUIET=0
LOG_FILE="${RALPH_LOG_FILE:-}"

for _arg in "$@"; do
    case "$_arg" in
        --minutes=*)
            _mins="${_arg#--minutes=}"
            case "$_mins" in ''|*[!0-9]*) die "--minutes requires a positive integer";; esac
            DURATION_SECS=$((DURATION_SECS + _mins * 60))
            ;;
        --hours=*)
            _hrs="${_arg#--hours=}"
            case "$_hrs" in ''|*[!0-9]*) die "--hours requires a positive integer";; esac
            DURATION_SECS=$((DURATION_SECS + _hrs * 3600))
            ;;
        --until=*)
            UNTIL_TASK="${_arg#--until=}"
            [ -n "$UNTIL_TASK" ] || die "--until requires a task id (e.g. --until=0.3.9)"
            ;;
        --dry-run) DRY_RUN=1 ;;
        --quiet|-q) QUIET=1 ;;
        --log=*)
            _logf="${_arg#--log=}"
            [ -n "$_logf" ] || die "--log requires a file path"
            LOG_FILE="$_logf"
            ;;
        -*)
            die "Unknown flag: $_arg  (supported: --minutes=N --hours=N --until=TASK --dry-run --quiet/-q --log=FILE)"
            ;;
        *)
            [ -z "$SINGLE_TASK" ] || die "Too many positional arguments — only one task id allowed"
            SINGLE_TASK="$_arg"
            ;;
    esac
done

START_TIME="$(date +%s)"
if [ "$DURATION_SECS" -gt 0 ]; then DEADLINE=$((START_TIME + DURATION_SECS)); else DEADLINE=0; fi

if [ -n "$LOG_FILE" ]; then
    touch "$LOG_FILE"
    exec > >(tee -a "$LOG_FILE") 2>&1
fi

# ── sanity checks ────────────────────────────────────────────────────────────
[ -f "$SKILL" ] || die "skill file missing: $SKILL"
command -v git >/dev/null 2>&1 || die "git not found in PATH"

# Branch validation: main or release/vX.Y only.
MINOR_VERSION=""
case "$BASE_BRANCH" in
    "$DEFAULT_BRANCH")
        MINOR_VERSION=""
        ;;
    release/v*)
        MINOR_VERSION="${BASE_BRANCH#release/v}"
        case "$MINOR_VERSION" in
            *[!0-9.]*|""|*.*.*) die "release branch '${BASE_BRANCH}' must be named release/vX.Y" ;;
        esac
        ;;
    *)
        die "ralph must be started from '${DEFAULT_BRANCH}' or a 'release/vX.Y' branch, not '${BASE_BRANCH}'.
  From ${DEFAULT_BRANCH} (multi-line — recommended): git checkout ${DEFAULT_BRANCH} && ./scripts/ralph.sh
  From one line:                                    git checkout release/v0.3 && ./scripts/ralph.sh"
        ;;
esac

# In single-task mode on a release branch, validate the task belongs here.
if [ -n "$SINGLE_TASK" ] && [ -n "$MINOR_VERSION" ]; then
    TASK_LINE="$(printf '%s' "$SINGLE_TASK" | sed 's/\.[0-9]*$//')"
    [ "$TASK_LINE" = "$MINOR_VERSION" ] \
        || die "Task ${SINGLE_TASK} belongs to line ${TASK_LINE}, not ${BASE_BRANCH}."
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
    die "working tree is dirty — commit or stash before running ralph"
fi

# ── PID file + stop mechanism ────────────────────────────────────────────────
RALPH_PID_FILE="/tmp/ralph.pid"
STOP_SENTINEL="$REPO_ROOT/scripts/STOP.md"
STOP_REQUESTED=0

printf '%d\n' $$ > "$RALPH_PID_FILE"
trap 'rm -f "$RALPH_PID_FILE"' EXIT
trap 'STOP_REQUESTED=1; warn "Stop signal received — will exit cleanly after the current task."' INT TERM

log "PID $$ written to $RALPH_PID_FILE"
if [ -n "$MINOR_VERSION" ]; then
    log "Mode: single-line  branch: ${BASE_BRANCH}"
else
    log "Mode: multi-line  starting from ${DEFAULT_BRANCH}"
fi
log "To stop gracefully: kill -TERM \$(cat $RALPH_PID_FILE) or touch $STOP_SENTINEL"

# ── agent helpers ────────────────────────────────────────────────────────────
agent_provider() { printf '%s' "$1" | sed 's|/.*||'; }
agent_model()    { printf '%s' "$1" | sed 's|^[^/]*/||'; }

agent_cli() {
    case "$(agent_provider "$1")" in
        claude-code)    printf 'claude' ;;
        github-copilot) printf 'copilot' ;;
        opencode-go)    printf 'opencode' ;;
        kilocode)       printf 'kilo' ;;
        *)              printf 'copilot' ;;
    esac
}

invoke_agent() {
    _agent="$1"; shift
    _prompt="$1"; shift

    if [ "$CAVEMAN" = "1" ]; then
        _cave="SPEAK IN CAVEMAN MODE (${CAVEMAN_LEVEL}). Ultra-compressed output. No fluff. Full technical accuracy. Short sentences.
"
        _prompt="${_cave}${_prompt}"
    fi

    _pf="$(mktemp 2>/dev/null || printf '/tmp/ralph-prompt-%s' "$$")"
    printf '%s' "$_prompt" > "$_pf"
    _cli="$(agent_cli "$_agent")"

    case "$(agent_provider "$_agent")" in
        claude-code)
            cat "$_pf" | "$_cli" -p --model "$(agent_model "$_agent")" \
                --dangerously-skip-permissions 2>&1 ;;
        github-copilot)
            if [ "$QUIET" = "1" ]; then
                cat "$_pf" | "$_cli" --model "$(agent_model "$_agent")" --allow-all --no-ask-user 2>/dev/null
            else
                cat "$_pf" | "$_cli" --model "$(agent_model "$_agent")" --allow-all --no-ask-user 2>&1
            fi ;;
        opencode-go)
            cat "$_pf" | "$_cli" run --model "$_agent" --dangerously-skip-permissions 2>&1 ;;
        *)
            cat "$_pf" | "$_cli" --model "$(agent_model "$_agent")" 2>&1 ;;
    esac
    rm -f "$_pf"
}

resolve_dev_agent() {
    _task_block="$1"
    _agent=$(printf '%s' "$_task_block" | grep -im1 'Agent:' | sed 's/.*Agent:\s*//' | xargs)
    case "$_agent" in
        task_planning_agent|TASK_PLANNING_AGENT)   printf '%s' "$TASK_PLANNING_AGENT" ;;
        basic_dev_agent|BASIC_DEV_AGENT)           printf '%s' "$BASIC_DEV_AGENT" ;;
        mid_dev_agent|MID_DEV_AGENT)               printf '%s' "$MID_DEV_AGENT" ;;
        pro_dev_agent|PRO_DEV_AGENT)               printf '%s' "$PRO_DEV_AGENT" ;;
        task_review_agent|TASK_REVIEW_AGENT)       printf '%s' "$TASK_REVIEW_AGENT" ;;
        release_review_agent|RELEASE_REVIEW_AGENT) printf '%s' "$RELEASE_REVIEW_AGENT" ;;
        major_release_review_agent|MAJOR_RELEASE_REVIEW_AGENT) printf '%s' "$MAJOR_RELEASE_REVIEW_AGENT" ;;
        architect_agent|ARCHITECT_AGENT)           printf '%s' "$ARCHITECT_AGENT" ;;
        human|Human|HUMAN)
            # Human-fenced tasks map to pro so they are visible; fence with --until.
            printf '%s' "$PRO_DEV_AGENT" ;;
        *)                                         printf '%s' "$MID_DEV_AGENT" ;;
    esac
}

# ── todo helpers ─────────────────────────────────────────────────────────────
all_todo_lines() {
    for _f in "$REPO_ROOT/docs"/$TODO_GLOB; do
        [ -f "$_f" ] && cat "$_f" || true
    done
}

# Next open task id within line X.Y (e.g. 0.3.7), or empty.
next_task() {
    all_todo_lines | grep -m1 "^- \[ \] \`${MINOR_VERSION}\.[0-9]\+\`" \
        | sed "s/^- \[ \] \`\([^\`]*\)\`.*/\1/" || true
}

# Release line of the first unchecked task across all files (e.g. 0.3).
next_minor() {
    all_todo_lines | grep -m1 "^- \[ \] \`[0-9]\+\.[0-9]\+\.[0-9]\+\`" \
        | sed "s/^- \[ \] \`\([^\`]*\)\`.*/\1/" \
        | sed 's/\.[0-9]*$//' || true
}
next_minor_probe() { next_minor; }

task_block() {
    TASK_ID="$1"
    all_todo_lines | awk -v tid="$TASK_ID" '
        BEGIN { pat = "^- \\[.\\] `" tid "`" }
        $0 ~ pat      { found=1; print; next }
        found && /^- \[.\] `[0-9]/ { exit }
        found         { print }
    '
}

ralph_log() {
    ENTRY="$1"
    mkdir -p "$(dirname "$LOG")"
    printf '\n## %s\n\n%s\n' "$(date '+%Y-%m-%d %H:%M')" "$ENTRY" >> "$LOG"
}

# Returns 0 (true) if semver A < B. Numeric per-component compare; missing
# components count as 0 (e.g. 0.3 < 0.3.9).
version_lt() {
    _a="$1"; _b="$2"
    printf '%s\n%s\n' "$_a" "$_b" | awk -F. '
        { for (i = 1; i <= 3; i++) v[NR, i] = ($i == "" ? 0 : $i + 0) }
        END {
            for (i = 1; i <= 3; i++) {
                if (v[1, i] < v[2, i]) { exit 0 }
                if (v[1, i] > v[2, i]) { exit 1 }
            }
            exit 1
        }'
}

auto_mark_task() {
    for _tf in "$REPO_ROOT/docs"/$TODO_GLOB; do
        [ -f "$_tf" ] || continue
        if grep -q "^- \[ \] \`${TASK_ID}\`" "$_tf" 2>/dev/null; then
            warn "Agent did not mark ${TASK_ID} as done — marking it now."
            if sed --version >/dev/null 2>&1; then
                sed -i "s|^- \[ \] \`${TASK_ID}\`|- [x] \`${TASK_ID}\`|" "$_tf"
            else
                sed -i '' "s|^- \[ \] \`${TASK_ID}\`|- [x] \`${TASK_ID}\`|" "$_tf"
            fi
            git add "$_tf"
            GIT_EDITOR=true git commit -m "chore(todo): auto-mark ${TASK_ID} done" >/dev/null 2>&1 \
                && git push origin "$BASE_BRANCH" 2>/dev/null || true
        fi
    done
}

# ── deadline / stop helpers ──────────────────────────────────────────────────
time_remaining() {
    _left=$((DEADLINE - $(date +%s)))
    if [ "$_left" -le 0 ]; then printf '0s'
    else printf '%dh %dm %ds' $(( _left / 3600 )) $(( (_left % 3600) / 60 )) $(( _left % 60 )); fi
}
deadline_reached() { [ "$DEADLINE" -gt 0 ] && [ "$(date +%s)" -ge "$DEADLINE" ]; }
stop_requested() {
    [ "$STOP_REQUESTED" -eq 1 ] && return 0
    if [ -f "$STOP_SENTINEL" ]; then
        warn "Stop sentinel found: $STOP_SENTINEL — consuming it."
        rm -f "$STOP_SENTINEL"; STOP_REQUESTED=1; return 0
    fi
    return 1
}

# ── branch helpers ───────────────────────────────────────────────────────────
switch_to_line() {
    _minor="$1"; _branch="release/v${_minor}"
    if git show-ref --verify --quiet "refs/heads/${_branch}" 2>/dev/null \
        || git ls-remote --exit-code --heads origin "${_branch}" >/dev/null 2>&1; then
        log "Switching to existing ${_branch}"
        git checkout "$_branch" >/dev/null 2>&1
        git pull --ff-only origin "$_branch" >/dev/null 2>&1 \
            || warn "could not fast-forward ${_branch} from origin — continuing local"
    else
        log "Creating ${_branch} from ${DEFAULT_BRANCH}"
        git checkout "$DEFAULT_BRANCH" >/dev/null 2>&1
        git pull --ff-only origin "$DEFAULT_BRANCH" >/dev/null 2>&1 \
            || warn "could not fast-forward ${DEFAULT_BRANCH} from origin — continuing local"
        git checkout -b "$_branch"
        git push -u origin "$_branch"
        good "${_branch} created and pushed."
    fi
    BASE_BRANCH="$_branch"; MINOR_VERSION="$_minor"
}

# ── semver gates ─────────────────────────────────────────────────────────────
# Y = 0 lines (e.g. 1.0) are release-gated: RC + human sign-off required.
is_major_release() {
    case "$MINOR_VERSION" in
        *.0) return 0 ;;
        *)   return 1 ;;
    esac
}

prepare_major_rc() {
    MAJOR="${MINOR_VERSION%.0}"
    RC_VER="${MAJOR}.0.0"
    RC_BRANCH="rc/v${RC_VER}-rc.1"
    RC_TAG="v${RC_VER}-rc.1"

    log "MAJOR RELEASE — line ${MINOR_VERSION} requires human sign-off."

    if git show-ref --verify --quiet "refs/heads/${RC_BRANCH}" 2>/dev/null; then
        git checkout "$RC_BRANCH" >/dev/null 2>&1
    else
        git checkout -b "$RC_BRANCH" >/dev/null 2>&1
        git push -u origin "$RC_BRANCH"
    fi
    if git show-ref --verify --quiet "refs/tags/${RC_TAG}" 2>/dev/null; then
        warn "Tag ${RC_TAG} already exists — skipping."
    else
        git tag -a "$RC_TAG" -m "Release candidate: ${RC_TAG}"
        git push origin "$RC_TAG"
    fi

    ralph_log "MAJOR_RC_READY: ${RC_BRANCH} + tag ${RC_TAG} created. Awaiting human sign-off."
    warn ""
    warn "  RC ready: ${RC_TAG} (branch ${RC_BRANCH})"
    warn "  Before merging to main:"
    warn "    1. Round-table review (>= 2 models)"
    warn "    2. Human sign-off"
    warn "    3. Human runs: git checkout ${DEFAULT_BRANCH} && git merge --no-ff ${RC_BRANCH}"
    warn ""
    exit 0
}

# ── phase completion review + merge ──────────────────────────────────────────
phase_review_and_merge() {
    if is_major_release; then prepare_major_rc; fi

    log "Line ${MINOR_VERSION} complete — running release review before merging."

    MAX_ATTEMPTS=3; ATTEMPT=0
    while [ $ATTEMPT -lt $MAX_ATTEMPTS ]; do
        ATTEMPT=$((ATTEMPT + 1))
        log "Review attempt ${ATTEMPT}/${MAX_ATTEMPTS}"

        LINE_STATUS="$(all_todo_lines | grep "\`${MINOR_VERSION}\." | head -30 || true)"
        CHANGED="$(git diff --name-only "${DEFAULT_BRANCH}"..."${BASE_BRANCH}" 2>/dev/null | head -100 || true)"
        COMMITS="$(git log --oneline "${DEFAULT_BRANCH}"..."${BASE_BRANCH}" 2>/dev/null | head -50 || true)"
        REVIEW_LOG="/tmp/ralph-line-review-${MINOR_VERSION}-${ATTEMPT}.log"

        invoke_agent "$RELEASE_REVIEW_AGENT" "You are performing a release-line completion review for the ${REPO_NAME} repository.
All tasks in line ${MINOR_VERSION} are reported complete. Inspect the repo as needed.

Branches: ${DEFAULT_BRANCH} vs ${BASE_BRANCH}

Task status:
${LINE_STATUS}

Commits:
${COMMITS}

Files changed:
${CHANGED}

Checklist — write PASS or FAIL plus one line each:
1. Every ${MINOR_VERSION}.* task is marked [x].
2. No regressions: checks pass (${FMT_CMD}; ${LINT_CMD}; ${TEST_CMD}).
3. docs/memory.md has entries covering this line where architectural choices were made.
4. No unrelated scope creep.
5. No security issues introduced.
6. Code quality acceptable — no dead code, no unwraps/panics in production paths.

If every item passes print exactly: PHASE_APPROVED
Otherwise print exactly: PHASE_BLOCKED and list what must be fixed first." \
            2>&1 | tee "$REVIEW_LOG" >/dev/null
        [ "$QUIET" = "1" ] || cat "$REVIEW_LOG"

        if grep -q "PHASE_APPROVED" "$REVIEW_LOG" 2>/dev/null; then
            good "Review approved — merging ${BASE_BRANCH} → ${DEFAULT_BRANCH}, tagging v${MINOR_VERSION}.0"
            git checkout "$DEFAULT_BRANCH"
            git pull --ff-only origin "$DEFAULT_BRANCH" >/dev/null 2>&1 || true
            git merge --no-ff "$BASE_BRANCH" \
                -m "release: merge ${BASE_BRANCH} into ${DEFAULT_BRANCH} — line ${MINOR_VERSION} complete"
            NEW_VER="${MINOR_VERSION}.0"
            if version_bump "$NEW_VER"; then
                GIT_EDITOR=true git commit -m "chore: bump version to ${NEW_VER}" >/dev/null 2>&1 || true
                log "Project version bumped to ${NEW_VER}"
            fi
            if ! git show-ref --verify --quiet "refs/tags/v${NEW_VER}" 2>/dev/null; then
                git tag -a "v${NEW_VER}" -m "v${NEW_VER}: release line ${MINOR_VERSION} complete"
                log "Tagged v${NEW_VER}"
            fi
            git push origin "$DEFAULT_BRANCH" --tags 2>/dev/null || git push origin "$DEFAULT_BRANCH"
            ralph_log "PHASE_COMPLETE: ${MINOR_VERSION} merged to ${DEFAULT_BRANCH}; tagged v${NEW_VER}."
            return 0
        fi

        warn "Review blocked (attempt ${ATTEMPT}/${MAX_ATTEMPTS}) — see ${REVIEW_LOG}"
        if [ $ATTEMPT -eq $MAX_ATTEMPTS ]; then
            ralph_log "PHASE_BLOCKED: ${MINOR_VERSION} review failed after ${MAX_ATTEMPTS} attempts. See ${REVIEW_LOG}."
            exit 1
        fi

        REVIEW_OUT="$(cat "$REVIEW_LOG")"
        log "Asking architect agent to fix review blockers…"
        invoke_agent "$ARCHITECT_AGENT" "The release review for line ${MINOR_VERSION} in ${REPO_NAME} returned PHASE_BLOCKED.
Fix every issue listed. Minimum changes only. Do NOT commit.
Print exactly: REVIEW_FIXES_DONE when finished.

Review output:
${REVIEW_OUT}" 2>&1 | tail -20

        if ! git diff --quiet || ! git diff --cached --quiet; then
            FIX_MSG="fix: address line ${MINOR_VERSION} review blockers (attempt ${ATTEMPT})"
            FIX_TRY=0
            while [ $FIX_TRY -lt 3 ]; do
                FIX_TRY=$((FIX_TRY + 1))
                git add -A
                if git commit -m "$FIX_MSG" >/dev/null 2>&1; then
                    good "Review fixes committed."
                    break
                fi
                if [ $FIX_TRY -eq 3 ]; then
                    warn "Fix commit failing after retries — continuing anyway."
                    break
                fi
                HOOK_ERR="$(git commit -m "$FIX_MSG" 2>&1 || true)"
                invoke_agent "$ARCHITECT_AGENT" "The commit for review fixes failed. Fix every failure below. Do NOT commit. Print exactly: FIXES_DONE when done.

Error output:
${HOOK_ERR}" 2>&1 | tail -10
            done
        fi
        log "Re-running review after fixes…"
    done
}

# ── per-task runner ──────────────────────────────────────────────────────────
run_task() {
    TASK_ID="$1"
    BRANCH="task/${TASK_ID}"

    log "Starting task ${BOLD}${TASK_ID}${RESET} on branch ${BRANCH}"

    git checkout "$BASE_BRANCH" >/dev/null 2>&1
    git pull --ff-only origin "$BASE_BRANCH" >/dev/null 2>&1 || true
    if ! git checkout -b "$BRANCH" >/dev/null 2>&1; then
        ralph_log "BLOCKED on ${TASK_ID}: branch ${BRANCH} already exists."
        die "Branch '${BRANCH}' already exists — resolve it manually, then re-run."
    fi

    TASK_BLOCK="$(task_block "$TASK_ID")"
    SKILL_TEXT="$(cat "$SKILL")"
    DEV_AGENT="$(resolve_dev_agent "$TASK_BLOCK")"

    CHECKS_BLOCK="Run these checks and make them pass before finishing:"
    [ -n "$FMT_CMD" ]  && CHECKS_BLOCK="${CHECKS_BLOCK}
  ${FMT_CMD}"
    [ -n "$LINT_CMD" ] && CHECKS_BLOCK="${CHECKS_BLOCK}
  ${LINT_CMD}"
    [ -n "$TEST_CMD" ] && CHECKS_BLOCK="${CHECKS_BLOCK}
  ${TEST_CMD}"

    # Step 1 — plan (no code).
    log "Step 1/3 — planning"
    PLAN="$(invoke_agent "$TASK_PLANNING_AGENT" "You are an expert engineer planning a task for the ${REPO_NAME} repository (${LANG_NAME}).
Read the skill file and task block, then write a numbered implementation plan. Do NOT write code.

TASK ID: ${TASK_ID}

TASK BLOCK:
${TASK_BLOCK}

SKILL FILE:
${SKILL_TEXT}

Produce:
1. Numbered list of files to create/edit (path + one-sentence purpose).
2. Numbered list of tests to write (name + what it proves).
3. Blockers or security concerns, if any.")"

    # Step 2 — implement.
    log "Step 2/3 — implementing with ${DEV_AGENT}"
    IMPL_LOG="/tmp/ralph-impl-${TASK_ID}.log"
    invoke_agent "$DEV_AGENT" "You are Ralph, the autonomous task agent for the ${REPO_NAME} repository.
Implement task ${TASK_ID} in full, following every rule in the skill file below.
Do not commit. Write all files, then verify your work.

${CHECKS_BLOCK}
Fix failures until clean. When finished print exactly: IMPLEMENTATION_DONE

TASK ID: ${TASK_ID}

TASK BLOCK:
${TASK_BLOCK}

PLAN:
${PLAN}

SKILL FILE:
${SKILL_TEXT}" 2>&1 | tee "$IMPL_LOG" | tail -15

    # Step 3 — self-review + fix.
    log "Step 3/3 — self-review"
    DIFF="$(git diff HEAD 2>/dev/null | head -600)"
    invoke_agent "$TASK_REVIEW_AGENT" "You are reviewing an implementation for the ${REPO_NAME} repository.
Work through the self-review checklist from the skill file. For each item write PASS
or FAIL plus one line. For any FAIL item, fix it in the code now.
Do not commit. After fixing everything print exactly: REVIEW_DONE

TASK ID: ${TASK_ID}

GIT DIFF (up to 600 lines):
${DIFF}

SKILL FILE (contains the checklist):
${SKILL_TEXT}" 2>&1 | tee "/tmp/ralph-review-${TASK_ID}.log" | tail -15

    # Commit with retry.
    log "Committing ${TASK_ID}"
    COMMIT_MSG="$(invoke_agent "$TASK_PLANNING_AGENT" "Write a conventional-commits message for task ${TASK_ID} in ${REPO_NAME}.
First line: '<type>(<scope>): <description, max 50 chars>'.
Blank line, then one short body paragraph (what + why). End with a footer line: Closes task ${TASK_ID}
Output only the message text, no fences.

Task:
${TASK_BLOCK}")"

    ATTEMPTS=0; MAX_COMMIT_ATTEMPTS=3
    COMMIT_LOG="/tmp/ralph-commit-${TASK_ID}.log"
    while [ $ATTEMPTS -lt $MAX_COMMIT_ATTEMPTS ]; do
        ATTEMPTS=$((ATTEMPTS + 1))
        log "Commit attempt ${ATTEMPTS}/${MAX_COMMIT_ATTEMPTS}"
        git add -A
        if git commit -m "$COMMIT_MSG" >"$COMMIT_LOG" 2>&1; then
            good "Commit succeeded."
            break
        fi
        if grep -q "nothing to commit" "$COMMIT_LOG" 2>/dev/null; then
            if git diff --quiet "${BASE_BRANCH}"...HEAD 2>/dev/null; then
                warn "Agent produced no changes — failing task ${TASK_ID}."
                ralph_log "FAILED: ${TASK_ID} — no changes produced."
                git checkout "$BASE_BRANCH" >/dev/null 2>&1
                git branch -D "$BRANCH" >/dev/null 2>&1 || true
                return 1
            fi
            good "Working tree clean — commit already exists."
            break
        fi
        cat "$COMMIT_LOG"
        if [ $ATTEMPTS -eq $MAX_COMMIT_ATTEMPTS ]; then
            ralph_log "BLOCKED on ${TASK_ID}: commit/checks still failing after ${MAX_COMMIT_ATTEMPTS} attempts."
            git checkout "$BASE_BRANCH" >/dev/null 2>&1
            git branch -D "$BRANCH" >/dev/null 2>&1 || true
            die "Giving up on ${TASK_ID} after ${MAX_COMMIT_ATTEMPTS} attempts."
        fi
        HOOK_OUT="$(cat "$COMMIT_LOG")"
        log "Asking architect agent to fix failures…"
        invoke_agent "$ARCHITECT_AGENT" "The commit for task ${TASK_ID} in ${REPO_NAME} failed its checks.
Fix every failure shown below. Change only what is required. Do NOT commit.
Print exactly: FIXES_DONE when done.

Failure output:
${HOOK_OUT}" 2>&1 | tee "/tmp/ralph-fix-${TASK_ID}-${ATTEMPTS}.log" | tail -15
    done

    # PR mode: push the task branch, open a PR, and stop — a human merges.
    if [ "$MERGE_MODE" = "pr" ] && command -v gh >/dev/null 2>&1; then
        git push -u origin "$BRANCH"
        PR_URL="$(gh pr create --base "$BASE_BRANCH" \
            --title "$(printf '%s' "$COMMIT_MSG" | head -1)" \
            --body "Closes task ${TASK_ID}

${COMMIT_MSG}" 2>&1 || true)"
        good "PR opened: ${PR_URL}"
        ralph_log "PR_OPENED: ${TASK_ID} — ${PR_URL}. Awaiting human merge."
        return 0
    fi

    # Merge back into the release line so todo state stays consistent.
    log "Merging ${BRANCH} into ${BASE_BRANCH}"
    git checkout "$BASE_BRANCH"
    git pull --ff-only origin "$BASE_BRANCH" >/dev/null 2>&1 || true
    git merge --no-ff "$BRANCH" -m "merge: ${BRANCH} into ${BASE_BRANCH}"

    auto_mark_task
    git push origin "$BASE_BRANCH"

    git branch -d "$BRANCH" >/dev/null 2>&1 || true
    git push origin --delete "$BRANCH" 2>/dev/null || true

    good "Task ${TASK_ID} merged into ${BASE_BRANCH}."
    ralph_log "DONE: ${TASK_ID} merged into ${BASE_BRANCH}."
}

# ── main ─────────────────────────────────────────────────────────────────────

# Dry-run: report the next action and exit (after helpers are defined).
if [ "$DRY_RUN" -eq 1 ]; then
    log "Dry run: no changes will be made."
    log "Repo: ${REPO_NAME} (${LANG_NAME})"
    log "Checks: fmt='${FMT_CMD}' lint='${LINT_CMD}' test='${TEST_CMD}'"
    if [ -n "$SINGLE_TASK" ]; then
        log "Would run task ${SINGLE_TASK} on $( [ -n "$MINOR_VERSION" ] && printf '%s' "$BASE_BRANCH" || printf 'release/v%s' "$(printf '%s' "$SINGLE_TASK" | sed 's/\.[0-9]*$//')" )."
        exit 0
    fi
    if [ -z "$MINOR_VERSION" ]; then
        _nm="$(next_minor)"
        if [ -n "$_nm" ]; then
            log "Would switch to release/v${_nm} and start its next open task."
        else
            log "No open tasks found."
        fi
    else
        _nt="$(next_task)"
        [ -n "$_nt" ] && log "Next task on ${BASE_BRANCH}: ${_nt}" \
                   || log "Line ${MINOR_VERSION} would go to review/merge."
    fi
    exit 0
fi

# Single-task mode.
if [ -n "$SINGLE_TASK" ]; then
    if deadline_reached || stop_requested; then
        warn "Stop/deadline condition met before task could start."
        exit 0
    fi
    if [ -z "$MINOR_VERSION" ]; then
        switch_to_line "$(printf '%s' "$SINGLE_TASK" | sed 's/\.[0-9]*$//')"
    fi
    run_task "$SINGLE_TASK"
    exit 0
fi

# Loop mode — works through every open line in sequence.
TASKS_DONE=0
while :; do
    if deadline_reached; then
        good "Time limit reached. Tasks completed: ${TASKS_DONE}."
        ralph_log "Time limit reached. Completed: ${TASKS_DONE}."
        exit 0
    fi
    if stop_requested; then
        good "Graceful stop. Tasks completed: ${TASKS_DONE}."
        ralph_log "Graceful stop. Completed: ${TASKS_DONE}."
        exit 0
    fi

    if [ "$BASE_BRANCH" = "$DEFAULT_BRANCH" ]; then
        git pull --ff-only origin "$DEFAULT_BRANCH" >/dev/null 2>&1 || true
        _nm="$(next_minor)"
        if [ -z "$_nm" ]; then
            good "All lines complete. Tasks completed: ${TASKS_DONE}."
            ralph_log "All lines complete. Completed: ${TASKS_DONE}."
            exit 0
        fi
        switch_to_line "$_nm"
    fi

    TASK_ID="$(next_task)"

    if [ -n "$UNTIL_TASK" ] && [ -n "$TASK_ID" ] && version_lt "$TASK_ID" "$UNTIL_TASK"; then
        good "Reached --until boundary (${UNTIL_TASK}). Stopping."
        ralph_log "--until boundary reached before ${TASK_ID}. Completed: ${TASKS_DONE}."
        exit 0
    fi

    if [ -z "$TASK_ID" ]; then
        good "Line ${MINOR_VERSION} complete (${TASKS_DONE} done this session)."
        if [ "$MERGE_MODE" = "pr" ]; then
            good "PR mode: open the line-completion PR yourself — ralph stops here."
            ralph_log "LINE_COMPLETE: ${MINOR_VERSION}. Human review + merge required (MERGE_MODE=pr)."
            exit 0
        fi
        phase_review_and_merge
        BASE_BRANCH="$DEFAULT_BRANCH"; MINOR_VERSION=""
        continue
    fi

    run_task "$TASK_ID" || {
        warn "Task ${TASK_ID} failed — logging and moving on."
        ralph_log "FAILED: ${TASK_ID} — see /tmp/ralph-*.log."
        git checkout "$BASE_BRANCH" >/dev/null 2>&1 || true
        git branch -D "task/${TASK_ID}" >/dev/null 2>&1 || true
    }

    if [ "$MERGE_MODE" = "pr" ]; then
        # Tasks are usually dependent — stop after each PR.
        good "PR mode: stopping after one task/PR. Re-run for the next task."
        exit 0
    fi

    if [ "$TASK_ID" = "$UNTIL_TASK" ]; then
        good "--until target ${UNTIL_TASK} completed. Stopping."
        ralph_log "--until target ${UNTIL_TASK} completed. Completed: ${TASKS_DONE}."
        exit 0
    fi

    TASKS_DONE=$((TASKS_DONE + 1))
    if [ "$DEADLINE" -gt 0 ]; then log "Time remaining: $(time_remaining)"; fi
    sleep 2
done
