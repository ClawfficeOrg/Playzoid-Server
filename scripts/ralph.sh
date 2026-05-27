#!/bin/sh
# ralph — autonomous task loop for the Playzoid-Server repository.
#
# Usage:
#   ./scripts/ralph.sh                     # pick next open task from main
#   ./scripts/ralph.sh 0.2-1              # single specific task
#   ./scripts/ralph.sh --minutes=30        # loop for up to 30 minutes
#   ./scripts/ralph.sh --hours=2           # loop for up to 2 hours
#   ./scripts/ralph.sh --hours=1 --minutes=30
#   ./scripts/ralph.sh --minutes=45 0.2-4  # single task, time-bounded
#   ./scripts/ralph.sh --loop              # keep picking tasks until none remain
#                                           # (use only when tasks are independent)
#
# Ralph MUST be started from the `main` branch with a clean working tree.
#
# For each open task ralph:
#   1. Creates branch `task/<id>` from main.
#   2. Invokes copilot with skills/ralph.md to implement the task.
#   3. Runs cargo fmt --check, cargo clippy -D warnings, cargo test.
#   4. Commits + pushes the task branch.
#   5. Opens a PR via `gh pr create` (never self-merges).
#   6. Marks the task `📬 PR #<n>` in docs/TODO.md.
#   7. Logs to docs/ralph-log.md and exits.
#
# Because tasks are typically dependent, ralph exits after each PR by default.
# Use --loop only for phases where tasks are truly independent.
#
# Stopping ralph gracefully (finishes current task first):
#   touch scripts/STOP.md
#   kill -TERM $(cat /tmp/ralph.pid)
#   Ctrl-C
#
# Requires: copilot CLI, gh CLI, git, cargo, a clean working tree.
# LOCALE: must be UTF-8 to correctly match ⏳/📬/✅ emoji in docs/TODO.md.

set -e

REPO_ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
SKILL="$REPO_ROOT/skills/ralph.md"
TODO="$REPO_ROOT/docs/TODO.md"
LOG="$REPO_ROOT/docs/ralph-log.md"
CHEAP_MODEL="${COPILOT_CHEAP_MODEL:-gpt-5-mini}"
CODE_MODEL="${COPILOT_MODEL:-claude-sonnet-4.6}"

# ── colours ──────────────────────────────────────────────────────────────────
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

# ── argument parsing ──────────────────────────────────────────────────────────
SINGLE_TASK=""
DURATION_SECS=0
LOOP_MODE=0

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
        --loop)
            LOOP_MODE=1
            ;;
        -*)
            die "Unknown flag: $_arg  (supported: --minutes=N  --hours=N  --loop)"
            ;;
        *)
            [ -z "$SINGLE_TASK" ] || die "Too many positional arguments — only one task id is allowed"
            SINGLE_TASK="$_arg"
            ;;
    esac
done

# Validate single-task id format: <phase>-<num>  e.g. 0.2-1, 1.0-14
if [ -n "$SINGLE_TASK" ]; then
    case "$SINGLE_TASK" in
        [0-9]*.[0-9]*-[0-9]*) : ;;  # ok
        *) die "Task id '$SINGLE_TASK' must be in format <phase>-<num> (e.g. 0.2-1)" ;;
    esac
fi

START_TIME="$(date +%s)"
if [ "$DURATION_SECS" -gt 0 ]; then
    DEADLINE=$((START_TIME + DURATION_SECS))
else
    DEADLINE=0
fi

# ── sanity checks ─────────────────────────────────────────────────────────────
[ -f "$SKILL" ] || die "skill file missing: $SKILL"
[ -f "$TODO" ]  || die "TODO file missing: $TODO"
command -v copilot >/dev/null 2>&1 || die "copilot CLI not found in PATH"
command -v gh     >/dev/null 2>&1 || die "gh CLI not found in PATH (needed for PR creation)"
command -v cargo  >/dev/null 2>&1 || die "cargo not found in PATH"

# Ralph must start from main.
BASE_BRANCH="$(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD)"
[ "$BASE_BRANCH" = "main" ] || die \
    "ralph must be started from 'main', not '${BASE_BRANCH}'.
  git checkout main && git pull --ff-only origin main
  ./scripts/ralph.sh"

cd "$REPO_ROOT"

if ! git diff --quiet || ! git diff --cached --quiet; then
    die "working tree is dirty — commit or stash changes before running ralph"
fi

# ── PID file + stop mechanism ─────────────────────────────────────────────────
RALPH_PID_FILE="/tmp/ralph.pid"
STOP_SENTINEL="$REPO_ROOT/scripts/STOP.md"
STOP_REQUESTED=0

printf '%d\n' $$ > "$RALPH_PID_FILE"
trap 'rm -f "$RALPH_PID_FILE"' EXIT
trap 'STOP_REQUESTED=1; warn "Stop signal received — finishing current task then exiting."' INT TERM

# ── announce ──────────────────────────────────────────────────────────────────
log "PID $$ written to $RALPH_PID_FILE"
log "To stop gracefully:  kill -TERM \$(cat $RALPH_PID_FILE)  or  touch $STOP_SENTINEL"

if [ "$DURATION_SECS" -gt 0 ]; then
    _human="$(( DURATION_SECS / 3600 ))h $(( (DURATION_SECS % 3600) / 60 ))m"
    log "Time limit: ${_human}"
fi

if [ "$LOOP_MODE" -eq 1 ]; then
    warn "Loop mode enabled — ralph will continue after each PR. Use only for independent tasks."
fi

# ── helpers ───────────────────────────────────────────────────────────────────

# Escape dots in a string for use in sed/grep patterns.
escape_dots() { printf '%s' "$1" | sed 's/\./\\./g'; }

# Extract phase (X.Y) from a task id (X.Y-N).
task_phase() { printf '%s' "$1" | sed 's/-[0-9][0-9]*$//'; }

# Determine the current active phase: the phase of the first ⏳ pending task.
# Returns empty string if no open tasks remain.
current_phase() {
    grep -m1 "^| *[0-9][0-9]*\.[0-9][0-9]*-[0-9][0-9]* *| *⏳" "$TODO" \
        | sed 's/^| *\([0-9][0-9]*\.[0-9][0-9]*\)-[0-9][0-9]* *|.*/\1/' \
        || true
}

# Return the next open task id (⏳ pending) for a given phase, or empty string.
next_task() {
    _ph="$1"
    _esc="$(escape_dots "$_ph")"
    grep -m1 "^| *${_esc}-[0-9][0-9]* *| *⏳" "$TODO" \
        | sed 's/^| *\([0-9][0-9]*\.[0-9][0-9]*-[0-9][0-9]*\) *|.*/\1/' \
        | sed 's/ *$//' \
        || true
}

# Extract the phase section from TODO.md (from ## Phase X.Y.* to the next ##).
# Provides ralph's copilot prompt with full context for the active phase.
phase_section() {
    _ph="$1"
    _esc="$(escape_dots "$_ph")"
    awk -v ph="$_esc" '
        /^## Phase / {
            if (found) exit
            if ($0 ~ ("Phase " ph "\\.")) found=1
        }
        found { print }
    ' "$TODO"
}

# Return the task definition row from the Tasks table (4-column row with
# description, complexity, agent — not the status row which starts with ⏳/✅).
task_definition_row() {
    _id="$1"
    _esc="$(escape_dots "$_id")"
    # The task definition row has the description in the 2nd column (starts
    # with a letter or backtick), unlike the status row (starts with emoji).
    grep "^| *${_esc} *| *[A-Za-z\`]" "$TODO" | head -1 || true
}

# Append a timestamped entry to the ralph log.
ralph_log() {
    _entry="$1"
    mkdir -p "$(dirname "$LOG")"
    printf '\n## %s\n\n%s\n' "$(date '+%Y-%m-%d %H:%M')" "$_entry" >> "$LOG"
}

# Update the status column for a task row in TODO.md (portable, no sed -i).
# Usage: set_task_status <task-id> <new-status-text>
set_task_status() {
    _id="$1"
    _status="$2"
    _esc="$(escape_dots "$_id")"
    _tmp="$(mktemp)"
    # Replace the status column (between first and second | after the task id)
    # Works for both ⏳ pending and 🟡 partial rows.
    sed "s/^| *${_esc} *| *[^|]*/| ${_id} | ${_status} /" "$TODO" > "$_tmp"
    mv "$_tmp" "$TODO"
}

# ── deadline / stop helpers ───────────────────────────────────────────────────
deadline_reached() {
    [ "$DEADLINE" -gt 0 ] && [ "$(date +%s)" -ge "$DEADLINE" ]
}

stop_requested() {
    [ "$STOP_REQUESTED" -eq 1 ] && return 0
    if [ -f "$STOP_SENTINEL" ]; then
        warn "Stop sentinel found: $STOP_SENTINEL — consuming it."
        rm -f "$STOP_SENTINEL"
        STOP_REQUESTED=1
        return 0
    fi
    return 1
}

# ── invoke copilot safely ─────────────────────────────────────────────────────
# Writes prompt to a temp file and pipes it to avoid argument length limits.
# Usage: invoke_copilot "$PROMPT" [extra copilot args...]
invoke_copilot() {
    _prompt="$1"; shift
    _pf="$(mktemp)"
    printf '%s' "$_prompt" > "$_pf"
    cat "$_pf" | copilot "$@" 2>/dev/null
    rm -f "$_pf"
}

# ── check gate ────────────────────────────────────────────────────────────────
# Run the three mandatory checks. Retries up to MAX_CHECK_ATTEMPTS times,
# asking copilot to fix failures between attempts.
MAX_CHECK_ATTEMPTS=3
run_checks_with_retry() {
    _attempt=0
    while [ $_attempt -lt $MAX_CHECK_ATTEMPTS ]; do
        _attempt=$((_attempt + 1))
        log "Check attempt ${_attempt}/${MAX_CHECK_ATTEMPTS}"

        _fmt_out="$(cargo fmt --check 2>&1)" && _fmt_ok=1 || _fmt_ok=0
        _clip_out="$(cargo clippy --all-targets --all-features -- -D warnings 2>&1)" && _clip_ok=1 || _clip_ok=0
        _test_out="$(cargo test 2>&1)" && _test_ok=1 || _test_ok=0

        if [ "$_fmt_ok" -eq 1 ] && [ "$_clip_ok" -eq 1 ] && [ "$_test_ok" -eq 1 ]; then
            good "All checks passed."
            # Capture last 20 lines of test output for PR body
            LAST_TEST_OUTPUT="$(printf '%s' "$_test_out" | tail -20)"
            return 0
        fi

        warn "Check failures on attempt ${_attempt}:"
        [ "$_fmt_ok"  -eq 0 ] && warn "  cargo fmt --check failed"
        [ "$_clip_ok" -eq 0 ] && warn "  cargo clippy failed"
        [ "$_test_ok" -eq 0 ] && warn "  cargo test failed"

        if [ $_attempt -lt $MAX_CHECK_ATTEMPTS ]; then
            log "Asking copilot to fix failures..."
            _fix_prompt="$(cat "$SKILL")

---

You are fixing check failures in the Playzoid-Server Rust codebase.
Repository: $REPO_ROOT

cargo fmt --check output:
$_fmt_out

cargo clippy output:
$_clip_out

cargo test output (last 60 lines):
$(printf '%s' "$_test_out" | tail -60)

Fix ONLY what is failing. Do not change unrelated code. Do not expand scope.
Apply all fixes now using your file editing tools."

            invoke_copilot "$_fix_prompt" \
                --model "$CODE_MODEL" \
                --add-dir "$REPO_ROOT/src" \
                --add-dir "$REPO_ROOT/tests" \
                >/dev/null
        fi
    done

    warn "Checks still failing after ${MAX_CHECK_ATTEMPTS} attempts."
    return 1
}

# ── single task execution ─────────────────────────────────────────────────────
run_task() {
    TASK_ID="$1"
    PHASE="$(task_phase "$TASK_ID")"
    TASK_BRANCH="task/${TASK_ID}"

    log "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    log "Task: ${TASK_ID}  Phase: ${PHASE}  Branch: ${TASK_BRANCH}"
    log "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

    # Ensure we are on a clean main before branching.
    git checkout main >/dev/null 2>&1
    git pull --ff-only origin main >/dev/null 2>&1 \
        || warn "Could not fast-forward main from origin — continuing on local."

    # Create or reset the task branch.
    if git show-ref --verify --quiet "refs/heads/${TASK_BRANCH}" 2>/dev/null; then
        warn "Branch ${TASK_BRANCH} already exists — deleting and recreating."
        git branch -D "$TASK_BRANCH" >/dev/null 2>&1
    fi
    git checkout -b "$TASK_BRANCH" >/dev/null 2>&1
    good "Created branch ${TASK_BRANCH}"

    # Mark the task in-progress.
    set_task_status "$TASK_ID" "🔄 in-progress"
    git add docs/TODO.md
    git commit -m "chore(todo): mark task ${TASK_ID} in-progress" >/dev/null 2>&1 || true

    # Build the copilot implementation prompt.
    TASK_DEF="$(task_definition_row "$TASK_ID")"
    PHASE_CTX="$(phase_section "$PHASE")"

    IMPL_PROMPT="$(cat "$SKILL")

---

## Current Task

Task ID: ${TASK_ID}
Task definition row from docs/TODO.md:
${TASK_DEF}

Full phase section for context:
${PHASE_CTX}

---

## Your Job

Implement task ${TASK_ID} in full, following every rule in the skill above.
Repository root: ${REPO_ROOT}

Steps:
1. Research: read all relevant existing files (src/, tests/, migrations/, docs/).
2. Plan: outline what you will create/edit before writing any code.
3. Implement: write production code, then tests, then the CHANGES.md entry.
4. Update docs/TODO.md: the status row for ${TASK_ID} will be updated by the
   script after your work; focus on the code and tests.
5. DO NOT run cargo commands — the script will run checks after you finish.
6. DO NOT commit or push — the script will handle that.

Model policy: use ${CODE_MODEL} for all Rust/SQL/test code."

    log "Invoking copilot for implementation..."
    invoke_copilot "$IMPL_PROMPT" \
        --model "$CODE_MODEL" \
        --add-dir "$REPO_ROOT/src" \
        --add-dir "$REPO_ROOT/tests" \
        --add-dir "$REPO_ROOT/migrations" \
        --add-dir "$REPO_ROOT/docs" \
        --add-dir "$REPO_ROOT/memory" \
        >/dev/null

    log "Implementation complete. Running checks..."

    # Run checks with retry.
    if ! run_checks_with_retry; then
        ralph_log "BLOCKED on task ${TASK_ID}: checks still failing after ${MAX_CHECK_ATTEMPTS} attempts. Fix manually then re-run ralph."
        warn "Task ${TASK_ID} is blocked. See docs/ralph-log.md."
        # Restore status to pending so the next ralph run retries.
        set_task_status "$TASK_ID" "⏳ pending"
        git add docs/TODO.md
        git commit -m "chore(todo): unblock task ${TASK_ID} — checks failed" >/dev/null 2>&1 || true
        git checkout main >/dev/null 2>&1
        return 1
    fi

    # Stage everything and commit.
    git add -A

    # Build commit message using cheap model.
    COMMIT_MSG_PROMPT="Write a conventional-commits commit message for completing task ${TASK_ID} in the Playzoid-Server Rust project.

Task definition: ${TASK_DEF}

Rules:
- First line: <type>(<scope>): <short description> (max 72 chars)
- Blank line
- One paragraph body describing what was done and why
- Footer: Closes task ${TASK_ID}

Output ONLY the commit message text, nothing else."

    COMMIT_MSG="$(invoke_copilot "$COMMIT_MSG_PROMPT" --model "$CHEAP_MODEL" 2>/dev/null)" \
        || COMMIT_MSG="feat: implement task ${TASK_ID}

Closes task ${TASK_ID}"

    git commit -m "$COMMIT_MSG"
    good "Committed task ${TASK_ID}"

    # Push the branch.
    git push origin HEAD
    good "Pushed ${TASK_BRANCH} to origin"

    # Build PR body using cheap model.
    PR_BODY_PROMPT="Write a GitHub PR description for task ${TASK_ID} in the Playzoid-Server Rust project.

Task definition: ${TASK_DEF}

cargo test output (last 20 lines):
${LAST_TEST_OUTPUT}

The PR body must include:
1. ## Summary — one paragraph
2. ## Acceptance Criteria — checkboxes, each ticked [x]
3. ## Test Output — the cargo test lines above in a code block
4. Footer line: Closes task ${TASK_ID}

Output ONLY the PR body markdown, nothing else."

    PR_BODY="$(invoke_copilot "$PR_BODY_PROMPT" --model "$CHEAP_MODEL" 2>/dev/null)" \
        || PR_BODY="Implements task ${TASK_ID}.

$(printf '%s' "$LAST_TEST_OUTPUT" | awk 'BEGIN{print "```"}{print}END{print "```"}')

Closes task ${TASK_ID}"

    # Derive PR title from commit message first line.
    PR_TITLE="$(printf '%s' "$COMMIT_MSG" | head -1)"

    # Open the PR.
    log "Opening PR..."
    PR_URL="$(gh pr create \
        --base main \
        --title "$PR_TITLE" \
        --body "$PR_BODY" 2>&1)" || {
        warn "gh pr create failed: $PR_URL"
        ralph_log "BLOCKED on task ${TASK_ID}: gh pr create failed. Push succeeded; open PR manually."
        set_task_status "$TASK_ID" "⏳ pending"
        git push origin HEAD --force-with-lease >/dev/null 2>&1 || true
        return 1
    }

    PR_NUM="$(printf '%s' "$PR_URL" | grep -o '[0-9][0-9]*$' || true)"
    good "PR opened: $PR_URL"

    # Update TODO.md status to 📬 PR #<n>.
    git checkout main >/dev/null 2>&1
    git pull --ff-only origin main >/dev/null 2>&1 || true
    set_task_status "$TASK_ID" "📬 PR #${PR_NUM}"
    git add docs/TODO.md
    git commit -m "chore(todo): mark task ${TASK_ID} as 📬 PR #${PR_NUM}" >/dev/null 2>&1 || true
    git push origin HEAD >/dev/null 2>&1 || true

    # Log the outcome.
    ralph_log "DONE: ${TASK_ID} — PR #${PR_NUM} opened. Awaiting human review + merge.
Branch: ${TASK_BRANCH}
PR: ${PR_URL}"

    good "Task ${TASK_ID} complete. PR #${PR_NUM} is open and awaiting review."
    return 0
}

# ── main loop ─────────────────────────────────────────────────────────────────
TASKS_COMPLETED=0
TASKS_ATTEMPTED=0

if [ -n "$SINGLE_TASK" ]; then
    # Single-task mode.
    TASKS_ATTEMPTED=$((TASKS_ATTEMPTED + 1))
    run_task "$SINGLE_TASK" && TASKS_COMPLETED=$((TASKS_COMPLETED + 1))
else
    # Multi-task mode: iterate through open tasks.
    while true; do
        # Check stop conditions.
        if stop_requested; then
            warn "Stopping — stop requested after ${TASKS_COMPLETED} task(s) completed."
            break
        fi
        if deadline_reached; then
            warn "Stopping — time limit reached after ${TASKS_COMPLETED} task(s) completed."
            break
        fi

        # Sync to latest main before finding next task.
        git checkout main >/dev/null 2>&1
        git pull --ff-only origin main >/dev/null 2>&1 || true

        PHASE="$(current_phase)"
        if [ -z "$PHASE" ]; then
            good "No more open tasks found in docs/TODO.md. All done!"
            break
        fi

        TASK_ID="$(next_task "$PHASE")"
        if [ -z "$TASK_ID" ]; then
            good "Phase ${PHASE} has no more ⏳ pending tasks."
            break
        fi

        TASKS_ATTEMPTED=$((TASKS_ATTEMPTED + 1))
        run_task "$TASK_ID" && TASKS_COMPLETED=$((TASKS_COMPLETED + 1))

        if [ "$LOOP_MODE" -eq 0 ]; then
            log "Stopping after one task (default). Use --loop to continue automatically."
            log "Re-run ralph after the PR is reviewed and merged to pick up the next task."
            break
        fi
    done
fi

ralph_log "Session complete. Tasks completed: ${TASKS_COMPLETED} of ${TASKS_ATTEMPTED} attempted."
log "Done. Tasks completed: ${TASKS_COMPLETED}."
