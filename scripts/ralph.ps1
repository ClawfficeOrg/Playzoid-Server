#!/usr/bin/env pwsh
# ralph.ps1 — PowerShell port of ralph.sh for Windows (and anywhere pwsh runs).
# Autonomous task agent for the Playzoid-Server repository.
#
# Usage:
#   .\scripts\ralph.ps1                     # pick next open task from main
#   .\scripts\ralph.ps1 0.2-1              # single specific task
#   .\scripts\ralph.ps1 --minutes=30        # loop for up to 30 minutes
#   .\scripts\ralph.ps1 --hours=2           # loop for up to 2 hours
#   .\scripts\ralph.ps1 --hours=1 --minutes=30
#   .\scripts\ralph.ps1 --minutes=45 0.2-4  # single task, time-bounded
#   .\scripts\ralph.ps1 --loop              # keep picking tasks (independent tasks only)
#
# Requires: copilot CLI, gh CLI, git, cargo. Must start from main branch.
# Never self-merges. All PRs require human approval.
#
# Stopping gracefully:
#   New-Item scripts\STOP.md -Force
#   Stop-Process -Id (Get-Content $env:TEMP\ralph.pid)
#   Ctrl-C

#Requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Strip VS Code copilot wrapper from PATH to avoid pipe encoding bugs.
$env:PATH = ($env:PATH -split ';' |
    Where-Object { $_ -notmatch 'copilotCli' }) -join ';'

# ── repo root ─────────────────────────────────────────────────────────────────
$RepoRoot = & git -C $PSScriptRoot rev-parse --show-toplevel 2>&1
if ($LASTEXITCODE -ne 0) { Write-Error "Not inside a git repository."; exit 1 }
$RepoRoot = $RepoRoot.Trim()

$SkillFile = Join-Path $RepoRoot "skills/ralph.md"
$TodoFile  = Join-Path $RepoRoot "docs/TODO.md"
$LogFile   = Join-Path $RepoRoot "docs/ralph-log.md"
$TempDir   = $env:TEMP

$CheapModel = if ($env:COPILOT_CHEAP_MODEL) { $env:COPILOT_CHEAP_MODEL } else { "gpt-5-mini" }
$CodeModel  = if ($env:COPILOT_MODEL)       { $env:COPILOT_MODEL       } else { "claude-sonnet-4.6" }

# ── colour helpers ────────────────────────────────────────────────────────────
function log  { param([string]$m) Write-Host "[ralph] $m" -ForegroundColor Cyan }
function good { param([string]$m) Write-Host "[ralph] $m" -ForegroundColor Green }
function warn { param([string]$m) Write-Host "[ralph] $m" -ForegroundColor Yellow }
function die  { param([string]$m) Write-Host "[ralph] $m" -ForegroundColor Red; exit 1 }

# ── copilot invocation ────────────────────────────────────────────────────────
function Invoke-Copilot {
    param(
        [string]$Prompt,
        [Parameter(ValueFromRemainingArguments = $true)]
        [string[]]$ExtraArgs = @()
    )
    if ($PSVersionTable.PSVersion.Major -ge 7) {
        $copilotExe = (Get-Command copilot -ErrorAction Stop).Source
        $psi = [System.Diagnostics.ProcessStartInfo]::new($copilotExe)
        $psi.ArgumentList.Add("-p")
        $psi.ArgumentList.Add($Prompt)
        foreach ($a in $ExtraArgs) { $psi.ArgumentList.Add($a) }
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError  = $true
        $psi.UseShellExecute        = $false
        $psi.WorkingDirectory       = $RepoRoot
        $proc    = [System.Diagnostics.Process]::Start($psi)
        $outTask = $proc.StandardOutput.ReadToEndAsync()
        $proc.StandardError.ReadToEnd() | Out-Null
        $proc.WaitForExit()
        return ($outTask.Result -split "`r?`n")
    }
    else {
        & copilot -p $Prompt @ExtraArgs 2>$null
    }
}

# ── argument parsing ──────────────────────────────────────────────────────────
$SingleTask   = ""
$DurationSecs = 0
$LoopMode     = $false

foreach ($a in $args) {
    if      ($a -match '^--minutes=(\d+)$') { $DurationSecs += [int]$Matches[1] * 60 }
    elseif  ($a -match '^--hours=(\d+)$')   { $DurationSecs += [int]$Matches[1] * 3600 }
    elseif  ($a -eq '--loop')               { $LoopMode = $true }
    elseif  ($a -match '^-')                { die "Unknown flag: $a  (supported: --minutes=N  --hours=N  --loop)" }
    else {
        if ($SingleTask -ne "") { die "Too many positional arguments — only one task id is allowed" }
        $SingleTask = $a
    }
}

if ($SingleTask -ne "" -and $SingleTask -notmatch '^\d+\.\d+-\d+$') {
    die "Task id '$SingleTask' must be in format <phase>-<num> (e.g. 0.2-1)"
}

$StartTime = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
$Deadline  = if ($DurationSecs -gt 0) { $StartTime + $DurationSecs } else { 0 }

# ── sanity checks ─────────────────────────────────────────────────────────────
if (-not (Test-Path $SkillFile)) { die "skill file missing: $SkillFile" }
if (-not (Test-Path $TodoFile))  { die "TODO file missing: $TodoFile" }
if (-not (Get-Command copilot -ErrorAction SilentlyContinue)) { die "copilot CLI not found in PATH" }
if (-not (Get-Command gh      -ErrorAction SilentlyContinue)) { die "gh CLI not found in PATH" }
if (-not (Get-Command cargo   -ErrorAction SilentlyContinue)) { die "cargo not found in PATH" }

$script:BaseBranch = (& git -C $RepoRoot rev-parse --abbrev-ref HEAD).Trim()
if ($script:BaseBranch -ne "main") {
    die "ralph must be started from 'main', not '$($script:BaseBranch)'.`n  git checkout main`n  .\scripts\ralph.ps1"
}

Set-Location $RepoRoot

& git diff --quiet 2>&1 | Out-Null; $d1 = $LASTEXITCODE
& git diff --cached --quiet 2>&1 | Out-Null; $d2 = $LASTEXITCODE
if ($d1 -ne 0 -or $d2 -ne 0) { die "working tree is dirty — commit or stash changes before running ralph" }

# ── PID file + stop mechanism ─────────────────────────────────────────────────
$PidFile        = Join-Path $TempDir "ralph.pid"
$StopSentinel   = Join-Path $RepoRoot "scripts/STOP.md"
$script:StopRequested = $false

Set-Content -Path $PidFile -Value ([string]$PID) -Encoding ASCII -Force

[Console]::add_CancelKeyPress([ConsoleCancelEventHandler]{
    param($src, $ev)
    $script:StopRequested = $true
    $ev.Cancel = $true
    Write-Host "`n[ralph] Stop signal received — finishing current task then exiting." -ForegroundColor Yellow
})

Register-EngineEvent -SourceIdentifier ([System.Management.Automation.PsEngineEvent]::Exiting) -Action {
    Remove-Item $PidFile -Force -ErrorAction SilentlyContinue
} | Out-Null

# ── announce ──────────────────────────────────────────────────────────────────
log "PID $PID written to $PidFile"
log "To stop gracefully:  Stop-Process -Id (Get-Content $PidFile)  or  New-Item $StopSentinel"
if ($DurationSecs -gt 0) {
    $h = [math]::Floor($DurationSecs / 3600)
    $m = [math]::Floor(($DurationSecs % 3600) / 60)
    log "Time limit: ${h}h ${m}m"
}
if ($LoopMode) { warn "Loop mode enabled — use only for independent tasks." }

# ── helpers ───────────────────────────────────────────────────────────────────

function Get-TodoLines {
    if (Test-Path $TodoFile) { [string[]](Get-Content $TodoFile) } else { @() }
}

# Return phase (X.Y) from task id (X.Y-N).
function Get-TaskPhase { param([string]$TaskId) $TaskId -replace '-\d+$', '' }

# Return the next ⏳ pending task id for a given phase, or empty string.
function Get-NextTask {
    param([string]$Phase)
    $esc  = [regex]::Escape($Phase)
    $pat  = "^\| *${esc}-\d+ *\| *⏳"
    $line = (Get-TodoLines) | Where-Object { $_ -match $pat } | Select-Object -First 1
    if ($line -and $line -match "^\| *(\d+\.\d+-\d+) *\|") { $Matches[1] } else { "" }
}

# Return the phase (X.Y) of the first ⏳ pending task in any phase, or empty.
function Get-CurrentPhase {
    $line = (Get-TodoLines) |
        Where-Object { $_ -match "^\| *\d+\.\d+-\d+ *\| *⏳" } |
        Select-Object -First 1
    if ($line -and $line -match "^\| *(\d+\.\d+)-\d+") { $Matches[1] } else { "" }
}

# Return the task definition row (4-column: id, description, complexity, agent).
function Get-TaskDefinitionRow {
    param([string]$TaskId)
    $esc = [regex]::Escape($TaskId)
    (Get-TodoLines) |
        Where-Object { $_ -match "^\| *${esc} *\| *[A-Za-z``]" } |
        Select-Object -First 1
}

# Return the phase section text from TODO.md.
function Get-PhaseSection {
    param([string]$Phase)
    $esc    = [regex]::Escape($Phase)
    $lines  = Get-TodoLines
    $result = [System.Collections.Generic.List[string]]::new()
    $found  = $false
    foreach ($line in $lines) {
        if ($line -match '^## Phase ') {
            if ($found) { break }
            if ($line -match "Phase ${esc}\.") { $found = $true }
        }
        if ($found) { $result.Add($line) }
    }
    $result -join "`n"
}

# Update the status column for a task row in TODO.md (atomic temp-file write).
function Set-TaskStatus {
    param([string]$TaskId, [string]$Status)
    $esc  = [regex]::Escape($TaskId)
    $raw  = Get-Content $TodoFile -Raw -ErrorAction SilentlyContinue
    $new  = $raw -replace "(?m)^\| *${esc} *\| *[^|]*", "| $TaskId | $Status "
    $tmp  = Join-Path $TempDir ("ralph-todo-" + [guid]::NewGuid().ToString() + ".tmp")
    [System.IO.File]::WriteAllText($tmp, $new, [System.Text.Encoding]::UTF8)
    Move-Item -Path $tmp -Destination $TodoFile -Force
}

# Append a timestamped entry to the ralph log.
function Write-RalphLog {
    param([string]$Entry)
    $ts      = Get-Date -Format "yyyy-MM-dd HH:mm"
    $content = "`n## $ts`n`n$Entry`n"
    $tmp     = Join-Path $TempDir ("ralph-log-" + [guid]::NewGuid().ToString() + ".tmp")
    $existing = if (Test-Path $LogFile) { Get-Content $LogFile -Raw -ErrorAction SilentlyContinue } else { "" }
    [System.IO.File]::WriteAllText($tmp, $existing + $content, [System.Text.Encoding]::UTF8)
    Move-Item -Path $tmp -Destination $LogFile -Force
}

# ── deadline / stop helpers ───────────────────────────────────────────────────
function Test-DeadlineReached {
    $Deadline -gt 0 -and [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() -ge $Deadline
}

function Test-StopRequested {
    if ($script:StopRequested) { return $true }
    if (Test-Path $StopSentinel) {
        warn "Stop sentinel found: $StopSentinel — consuming it."
        Remove-Item $StopSentinel -Force
        $script:StopRequested = $true
        return $true
    }
    $false
}

# ── check gate with retry ─────────────────────────────────────────────────────
$script:LastTestOutput = ""
$MaxCheckAttempts      = 3

function Invoke-ChecksWithRetry {
    for ($attempt = 1; $attempt -le $MaxCheckAttempts; $attempt++) {
        log "Check attempt $attempt/$MaxCheckAttempts"

        $fmtOut  = & cargo fmt --check 2>&1 | Out-String; $fmtOk  = $LASTEXITCODE -eq 0
        $clipOut = & cargo clippy --all-targets --all-features -- -D warnings 2>&1 | Out-String; $clipOk = $LASTEXITCODE -eq 0
        $testOut = & cargo test 2>&1 | Out-String; $testOk = $LASTEXITCODE -eq 0

        if ($fmtOk -and $clipOk -and $testOk) {
            good "All checks passed."
            $script:LastTestOutput = ($testOut -split "`r?`n" | Select-Object -Last 20) -join "`n"
            return $true
        }

        if (-not $fmtOk)  { warn "  cargo fmt --check failed" }
        if (-not $clipOk) { warn "  cargo clippy failed" }
        if (-not $testOk) { warn "  cargo test failed" }

        if ($attempt -lt $MaxCheckAttempts) {
            log "Asking copilot to fix failures..."
            $skill = Get-Content $SkillFile -Raw
            $fixPrompt = @"
$skill

---

You are fixing check failures in the Playzoid-Server Rust codebase.
Repository: $RepoRoot

cargo fmt --check output:
$fmtOut

cargo clippy output:
$clipOut

cargo test output (last 60 lines):
$(($testOut -split "`r?`n" | Select-Object -Last 60) -join "`n")

Fix ONLY what is failing. Do not change unrelated code. Do not expand scope.
Apply all fixes now using your file editing tools.
"@
            Invoke-Copilot $fixPrompt `
                "--model" $CodeModel `
                "--add-dir" (Join-Path $RepoRoot "src") `
                "--add-dir" (Join-Path $RepoRoot "tests") | Out-Null
        }
    }
    return $false
}

# ── single task execution ─────────────────────────────────────────────────────
function Invoke-Task {
    param([string]$TaskId)

    $phase      = Get-TaskPhase $TaskId
    $taskBranch = "task/$TaskId"

    log ("━" * 45)
    log "Task: $TaskId  Phase: $phase  Branch: $taskBranch"
    log ("━" * 45)

    & git checkout main 2>&1 | Out-Null
    & git pull --ff-only origin main 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { warn "Could not fast-forward main from origin — continuing on local." }

    # Create or reset branch.
    & git show-ref --verify --quiet "refs/heads/$taskBranch" 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) {
        warn "Branch $taskBranch already exists — deleting and recreating."
        & git branch -D $taskBranch 2>&1 | Out-Null
    }
    & git checkout -b $taskBranch 2>&1 | Out-Null
    good "Created branch $taskBranch"

    # Mark in-progress.
    Set-TaskStatus $TaskId "🔄 in-progress"
    & git add "docs/TODO.md"
    & git commit -m "chore(todo): mark task $TaskId in-progress" 2>&1 | Out-Null

    # Build implementation prompt.
    $taskDef    = Get-TaskDefinitionRow $TaskId
    $phaseCtx   = Get-PhaseSection $phase
    $skill      = Get-Content $SkillFile -Raw

    $implPrompt = @"
$skill

---

## Current Task

Task ID: $TaskId
Task definition row from docs/TODO.md:
$taskDef

Full phase section for context:
$phaseCtx

---

## Your Job

Implement task $TaskId in full, following every rule in the skill above.
Repository root: $RepoRoot

Steps:
1. Research: read all relevant existing files (src/, tests/, migrations/, docs/).
2. Plan: outline what you will create/edit before writing any code.
3. Implement: write production code, then tests, then the CHANGES.md entry.
4. DO NOT run cargo commands — the script will run checks after you finish.
5. DO NOT commit or push — the script will handle that.

Model policy: use $CodeModel for all Rust/SQL/test code.
"@

    log "Invoking copilot for implementation..."
    Invoke-Copilot $implPrompt `
        "--model" $CodeModel `
        "--add-dir" (Join-Path $RepoRoot "src") `
        "--add-dir" (Join-Path $RepoRoot "tests") `
        "--add-dir" (Join-Path $RepoRoot "migrations") `
        "--add-dir" (Join-Path $RepoRoot "docs") `
        "--add-dir" (Join-Path $RepoRoot "memory") | Out-Null

    log "Implementation complete. Running checks..."

    if (-not (Invoke-ChecksWithRetry)) {
        Write-RalphLog "BLOCKED on task $TaskId`: checks still failing after $MaxCheckAttempts attempts."
        warn "Task $TaskId is blocked. See docs/ralph-log.md."
        Set-TaskStatus $TaskId "⏳ pending"
        & git add "docs/TODO.md"
        & git commit -m "chore(todo): unblock task $TaskId — checks failed" 2>&1 | Out-Null
        & git checkout main 2>&1 | Out-Null
        return $false
    }

    # Commit.
    & git add -A

    $commitMsgPrompt = @"
Write a conventional-commits commit message for completing task $TaskId in the Playzoid-Server Rust project.

Task definition: $taskDef

Rules:
- First line: <type>(<scope>): <short description> (max 72 chars)
- Blank line
- One paragraph body describing what was done and why
- Footer: Closes task $TaskId

Output ONLY the commit message text, nothing else.
"@
    $commitMsg = (Invoke-Copilot $commitMsgPrompt "--model" $CheapModel 2>$null) -join "`n"
    if (-not $commitMsg) { $commitMsg = "feat: implement task $TaskId`n`nCloses task $TaskId" }

    & git commit -m $commitMsg
    good "Committed task $TaskId"

    & git push origin HEAD
    good "Pushed $taskBranch to origin"

    # Build PR body.
    $prBodyPrompt = @"
Write a GitHub PR description for task $TaskId in the Playzoid-Server Rust project.

Task definition: $taskDef

cargo test output (last 20 lines):
$($script:LastTestOutput)

The PR body must include:
1. ## Summary — one paragraph
2. ## Acceptance Criteria — checkboxes, each ticked [x]
3. ## Test Output — the cargo test lines in a code block
4. Footer line: Closes task $TaskId

Output ONLY the PR body markdown, nothing else.
"@
    $prBody = (Invoke-Copilot $prBodyPrompt "--model" $CheapModel 2>$null) -join "`n"
    if (-not $prBody) { $prBody = "Implements task $TaskId.`n`nCloses task $TaskId" }

    $prTitle = ($commitMsg -split "`n")[0]

    log "Opening PR..."
    $prOutput = & gh pr create --base main --title $prTitle --body $prBody 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) {
        warn "gh pr create failed: $prOutput"
        Write-RalphLog "BLOCKED on task $TaskId`: gh pr create failed. Push succeeded; open PR manually."
        Set-TaskStatus $TaskId "⏳ pending"
        & git push origin HEAD --force-with-lease 2>&1 | Out-Null
        return $false
    }

    $prUrl = $prOutput.Trim()
    $prNum = if ($prUrl -match '(\d+)$') { $Matches[1] } else { "?" }
    good "PR opened: $prUrl"

    # Update TODO.md on main.
    & git checkout main 2>&1 | Out-Null
    & git pull --ff-only origin main 2>&1 | Out-Null
    Set-TaskStatus $TaskId "📬 PR #$prNum"
    & git add "docs/TODO.md"
    & git commit -m "chore(todo): mark task $TaskId as 📬 PR #$prNum" 2>&1 | Out-Null
    & git push origin HEAD 2>&1 | Out-Null

    Write-RalphLog "DONE: $TaskId — PR #$prNum opened. Awaiting human review + merge.`nBranch: $taskBranch`nPR: $prUrl"
    good "Task $TaskId complete. PR #$prNum is open and awaiting review."
    return $true
}

# ── main loop ─────────────────────────────────────────────────────────────────
$TasksCompleted = 0
$TasksAttempted = 0

if ($SingleTask -ne "") {
    $TasksAttempted++
    if (Invoke-Task $SingleTask) { $TasksCompleted++ }
}
else {
    while ($true) {
        if (Test-StopRequested) {
            warn "Stopping — stop requested after $TasksCompleted task(s)."
            break
        }
        if (Test-DeadlineReached) {
            warn "Stopping — time limit reached after $TasksCompleted task(s)."
            break
        }

        & git checkout main 2>&1 | Out-Null
        & git pull --ff-only origin main 2>&1 | Out-Null

        $phase = Get-CurrentPhase
        if (-not $phase) {
            good "No more open tasks in docs/TODO.md. All done!"
            break
        }

        $taskId = Get-NextTask $phase
        if (-not $taskId) {
            good "Phase $phase has no more pending tasks."
            break
        }

        $TasksAttempted++
        if (Invoke-Task $taskId) { $TasksCompleted++ }

        if (-not $LoopMode) {
            log "Stopping after one task (default). Use --loop to continue automatically."
            log "Re-run ralph after the PR is reviewed and merged."
            break
        }
    }
}

Write-RalphLog "Session complete. Tasks completed: $TasksCompleted of $TasksAttempted attempted."
log "Done. Tasks completed: $TasksCompleted."
