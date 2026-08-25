#!/usr/bin/env pwsh
# ralph.ps1 — PowerShell port of ralph.sh (generic baseline, semver task ids).
# Autonomous task loop for this repository.
#
# Usage:
#   .\scripts\ralph.ps1                          # multi-line loop through all open release lines
#   .\scripts\ralph.ps1 0.3.5                    # single task (infers release branch)
#   .\scripts\ralph.ps1 -Minutes 30              # loop for up to 30 minutes
#   .\scripts\ralph.ps1 -Hours 2                 # loop for up to 2 hours
#   .\scripts\ralph.ps1 -Until 0.3.9             # run tasks up to and including an id
#   .\scripts\ralph.ps1 -DryRun                  # preview the next action and exit
#   .\scripts\ralph.ps1 -Quiet                   # suppress agent stderr
#
# Versioning (semver):
#   Task ids are X.Y.Z — Y = release line, Z = task number within the line.
#   Each line gets a `release/vX.Y` branch; each task a `task/vX.Y.Z` branch.
#   On line completion: review -> merge to main -> tag `vX.Y.0` -> bump project
#   version to exactly `X.Y.0`. Lines with Y = 0 require an RC tag + human
#   sign-off (ralph never auto-merges them).
#
# Stopping gracefully:
#   New-Item scripts\STOP.md -Force             # sentinel file
#   Stop-Process -Id (Get-Content $env:TEMP\ralph.pid)
#   Ctrl-C
#
# Requires: pwsh, git, an agent CLI (see AGENT configuration).

#Requires -Version 5.1
[CmdletBinding()]
param(
    [int]$Minutes = 0,
    [int]$Hours = 0,
    [string]$SingleTask = '',
    [string]$Until = '',
    [switch]$DryRun,
    [switch]$Quiet,
    [string]$Log = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ── repo root ─────────────────────────────────────────────────────────────────
$RepoRoot = (& git -C $PSScriptRoot rev-parse --show-toplevel 2>&1)
if ($LASTEXITCODE -ne 0) { Write-Error 'Not inside a git repository.'; exit 1 }
$RepoRoot = ($RepoRoot | Out-String).Trim()
Set-Location $RepoRoot

# ══════════════════════════════════════════════════════════════════════════
# CONFIG — per-repo adaptation happens HERE only.
# ══════════════════════════════════════════════════════════════════════════

$RepoName      = Split-Path -Leaf $RepoRoot
$SkillFile     = if ($env:SKILL) { $env:SKILL } else { Join-Path $RepoRoot 'skills/ralph.md' }
$LogFile       = Join-Path $RepoRoot 'docs/ralph-log.md'
$TodoGlob      = 'todo-v*.md'
$DefaultBranch = if ($env:DEFAULT_BRANCH) { $env:DEFAULT_BRANCH } else { 'main' }

# Check commands (green gate). Override via env: FMT_CMD / LINT_CMD / TEST_CMD.
if (Test-Path (Join-Path $RepoRoot 'Cargo.toml')) {
    $FmtCmd  = if ($env:FMT_CMD)  { $env:FMT_CMD }  else { 'cargo fmt --check' }
    $LintCmd = if ($env:LINT_CMD) { $env:LINT_CMD } else { 'cargo clippy --all-targets --all-features -- -D warnings' }
    $TestCmd = if ($env:TEST_CMD) { $env:TEST_CMD } else { 'cargo test' }
    $LangName = 'Rust'
}
elseif (Test-Path (Join-Path $RepoRoot 'package.json')) {
    $FmtCmd  = if ($env:FMT_CMD)  { $env:FMT_CMD }  else { 'npx prettier --check .' }
    $LintCmd = if ($env:LINT_CMD) { $env:LINT_CMD } else { 'npm run lint' }
    $TestCmd = if ($env:TEST_CMD) { $env:TEST_CMD } else { 'npm test' }
    $LangName = 'TypeScript/JavaScript'
}
elseif ((Test-Path (Join-Path $RepoRoot 'pyproject.toml')) -or (Test-Path (Join-Path $RepoRoot 'setup.py'))) {
    $FmtCmd  = if ($env:FMT_CMD)  { $env:FMT_CMD }  else { 'ruff format --check .' }
    $LintCmd = if ($env:LINT_CMD) { $env:LINT_CMD } else { 'ruff check .' }
    $TestCmd = if ($env:TEST_CMD) { $env:TEST_CMD } else { 'pytest' }
    $LangName = 'Python'
}
elseif (Test-Path (Join-Path $RepoRoot 'go.mod')) {
    $FmtCmd  = if ($env:FMT_CMD)  { $env:FMT_CMD }  else { 'gofmt -l .' }
    $LintCmd = if ($env:LINT_CMD) { $env:LINT_CMD } else { 'go vet ./...' }
    $TestCmd = if ($env:TEST_CMD) { $env:TEST_CMD } else { 'go test ./...' }
    $LangName = 'Go'
}
else {
    $FmtCmd = ''; $LintCmd = ''; $TestCmd = ''
    $LangName = 'unknown'
}

# Project version bump target. Returns $true when the version file was updated.
function Update-ProjectVersion([string]$NewVer) {
    $cargo = Join-Path $RepoRoot 'Cargo.toml'
    $pkgjson = Join-Path $RepoRoot 'package.json'
    $pyproject = Join-Path $RepoRoot 'pyproject.toml'
    $versionFile = Join-Path $RepoRoot 'VERSION'

    if ((Test-Path $cargo) -and (Select-String -Path $cargo -Pattern '^version = ' -Quiet)) {
        (Get-Content $cargo -Raw) -replace '(?m)^version = ".*"', "version = `"$NewVer`"" |
            Set-Content $cargo -NoNewline
        & git add Cargo.toml | Out-Null
        return $true
    }
    elseif (Test-Path $pkgjson) {
        $json = Get-Content $pkgjson -Raw | ConvertFrom-Json
        $json.version = $NewVer
        $json | ConvertTo-Json -Depth 100 | Set-Content $pkgjson
        & git add package.json | Out-Null
        return $true
    }
    elseif ((Test-Path $pyproject) -and (Select-String -Path $pyproject -Pattern '^version = ' -Quiet)) {
        (Get-Content $pyproject -Raw) -replace '(?m)^version = ".*"', "version = `"$NewVer`"" |
            Set-Content $pyproject -NoNewline
        & git add pyproject.toml | Out-Null
        return $true
    }
    elseif (Test-Path $versionFile) {
        Set-Content $versionFile "$NewVer`n" -NoNewline
        & git add VERSION | Out-Null
        return $true
    }
    return $false
}

# ══════════════════════════════════════════════════════════════════════════
# AGENT configuration — each agent is a PROVIDER/MODEL pair.
# Providers: opencode-go -> opencode, github-copilot -> copilot,
#            claude-code -> claude, kilocode -> kilo
# ══════════════════════════════════════════════════════════════════════════

$TaskPlanningAgent         = if ($env:TASK_PLANNING_AGENT) { $env:TASK_PLANNING_AGENT } else { 'opencode-go/deepseek-v4-flash' }
$BasicDevAgent             = if ($env:BASIC_DEV_AGENT)     { $env:BASIC_DEV_AGENT }     else { 'opencode-go/deepseek-v4-flash' }
$MidDevAgent               = if ($env:MID_DEV_AGENT)       { $env:MID_DEV_AGENT }       else { 'opencode-go/deepseek-v4-flash' }
$ProDevAgent               = if ($env:PRO_DEV_AGENT)       { $env:PRO_DEV_AGENT }       else { 'github-copilot/claude-sonnet-4.6' }
$TaskReviewAgent           = if ($env:TASK_REVIEW_AGENT)   { $env:TASK_REVIEW_AGENT }   else { 'opencode-go/deepseek-v4-flash' }
$ReleaseReviewAgent        = if ($env:RELEASE_REVIEW_AGENT) { $env:RELEASE_REVIEW_AGENT } else { 'github-copilot/claude-sonnet-4.6' }
$MajorReleaseReviewAgent   = if ($env:MAJOR_RELEASE_REVIEW_AGENT) { $env:MAJOR_RELEASE_REVIEW_AGENT } else { 'github-copilot/claude-opus-4.8' }
$ArchitectAgent            = if ($env:ARCHITECT_AGENT)     { $env:ARCHITECT_AGENT }     else { 'github-copilot/claude-sonnet-4.6' }

$Caveman       = if ($env:CAVEMAN -eq '1') { $true } else { $false }
$CavemanLevel  = if ($env:CAVEMAN_LEVEL)   { $env:CAVEMAN_LEVEL } else { 'full' }

# Merge policy: 'local' (Zoid-style direct merges) or 'pr' (open PR and stop).
$MergeMode = if ($env:MERGE_MODE) { $env:MERGE_MODE } else { 'local' }

$BaseBranch = (& git rev-parse --abbrev-ref HEAD | Out-String).Trim()
if ($BaseBranch -eq 'master') { $BaseBranch = $DefaultBranch }

# ── logging ───────────────────────────────────────────────────────────────────
function Write-Rlog([string]$Message)  { Write-Host "[ralph] $Message" -ForegroundColor Cyan }
function Write-Rgood([string]$Message) { Write-Host "[ralph] $Message" -ForegroundColor Green }
function Write-Rwarn([string]$Message) { Write-Host "[ralph] $Message" -ForegroundColor Yellow }
function Write-Rdie([string]$Message)  { Write-Host "[ralph] $Message" -ForegroundColor Red; exit 1 }

# ── sanity checks ─────────────────────────────────────────────────────────────
if (-not (Test-Path $SkillFile)) { Write-Rdie "skill file missing: $SkillFile" }
$MinorVersion = ''
if ($BaseBranch -eq $DefaultBranch) {
    $MinorVersion = ''
}
elseif ($BaseBranch -match '^release/v(\d+\.\d+)$') {
    $MinorVersion = $Matches[1]
}
else {
    Write-Rdie ("ralph must be started from '{0}' or a 'release/vX.Y' branch, not '{1}'." -f $DefaultBranch, $BaseBranch)
}

if ($SingleTask -and $MinorVersion) {
    $taskLine = $SingleTask -replace '\.\d+$', ''
    if ($taskLine -ne $MinorVersion) {
        Write-Rdie "Task $SingleTask belongs to line $taskLine, not $BaseBranch."
    }
}

$DurationSecs = ($Minutes * 60) + ($Hours * 3600)
$StartTime = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
$Deadline = 0
if ($DurationSecs -gt 0) { $Deadline = $StartTime + $DurationSecs }

# ── stop mechanism ────────────────────────────────────────────────────────────
$tmpDir = if ($env:TEMP) { $env:TEMP } elseif ($env:TMPDIR) { $env:TMPDIR } else { '/tmp' }
$RalphPidFile = Join-Path $tmpDir 'ralph.pid'
$StopSentinel = Join-Path $RepoRoot 'scripts/STOP.md'
$script:StopRequested = $false

Set-Content -Path $RalphPidFile -Value $PID

try { [Console]::TreatControlCAsInput = $false } catch {}

Write-Rlog "PID $PID written to $RalphPidFile"
if ($MinorVersion) { Write-Rlog "Mode: single-line  branch: $BaseBranch" }
else               { Write-Rlog "Mode: multi-line  starting from $DefaultBranch" }
Write-Rlog "To stop gracefully: Stop-Process -Id (Get-Content $RalphPidFile) or create $StopSentinel"

# ── agent helpers ─────────────────────────────────────────────────────────────
function Get-AgentProvider([string]$Agent) { ($Agent -split '/')[0] }
function Get-AgentModel([string]$Agent)    { ($Agent -split '/', 2)[1] }

function Get-AgentCli([string]$Provider) {
    switch ($Provider) {
        'claude-code'    { 'claude' }
        'github-copilot' { 'copilot' }
        'opencode-go'    { 'opencode' }
        'kilocode'       { 'kilo' }
        default          { 'copilot' }
    }
}

function Invoke-Agent([string]$Agent, [string]$Prompt) {
    if ($Caveman) {
        $Prompt = "SPEAK IN CAVEMAN MODE ($CavemanLevel). Ultra-compressed output. No fluff. Full technical accuracy.`n`n$Prompt"
    }
    $cli = Get-AgentCli (Get-AgentProvider $Agent)
    $model = Get-AgentModel $Agent
    try {
        switch (Get-AgentProvider $Agent) {
            'claude-code' {
                $Prompt | & $cli -p --model $model --dangerously-skip-permissions 2>&1
            }
            'github-copilot' {
                if ($Quiet) { $Prompt | & $cli --model $model --allow-all --no-ask-user 2>$null }
                else        { $Prompt | & $cli --model $model --allow-all --no-ask-user 2>&1 }
            }
            'opencode-go' {
                $Prompt | & $cli run --model $Agent --dangerously-skip-permissions 2>&1
            }
            default {
                $Prompt | & $cli --model $model 2>&1
            }
        }
    }
    catch {
        Write-Rwarn "agent invocation failed: $_"
    }
}

function Resolve-DevAgent([string]$TaskBlock) {
    $agentLine = ($TaskBlock -split "`n" | Select-String -Pattern 'Agent:\s*(\S+)').Matches.Groups[1].Value
    switch -Regex ($agentLine) {
        '^(task_planning|TASK_PLANNING)'    { return $TaskPlanningAgent }
        '^(basic_dev|BASIC_DEV)'            { return $BasicDevAgent }
        '^(mid_dev|MID_DEV)'                { return $MidDevAgent }
        '^(pro_dev|PRO_DEV)'                { return $ProDevAgent }
        '^(task_review|TASK_REVIEW)'        { return $TaskReviewAgent }
        '^(release_review|RELEASE_REVIEW)'  { return $ReleaseReviewAgent }
        '^(major_release|MAJOR_RELEASE)'    { return $MajorReleaseReviewAgent }
        '^(architect|ARCHITECT)'            { return $ArchitectAgent }
        '^(human|Human|HUMAN)$'             { return $ProDevAgent }
        default                             { return $MidDevAgent }
    }
}

# ── todo helpers ──────────────────────────────────────────────────────────────
function Get-AllTodoLines {
    $lines = @()
    Get-ChildItem -Path (Join-Path $RepoRoot 'docs') -Filter $TodoGlob -File -ErrorAction SilentlyContinue |
        ForEach-Object { $lines += (Get-Content $_.FullName) }
    return $lines
}

function Get-NextTask([string]$LinePrefix) {
    $hit = Get-AllTodoLines | Where-Object { $_ -match ('^- \[ \] `' + [regex]::Escape($LinePrefix) + '\.\d+`') } |
        Select-Object -First 1
    if ($hit -match '^- \[ \] `([^`]+)`') { return $Matches[1] }
    return ''
}

function Get-NextMinor {
    $hit = Get-AllTodoLines | Where-Object { $_ -match '^- \[ \] `(\d+\.\d+)\.\d+`' } | Select-Object -First 1
    if ($hit -match '`(\d+\.\d+)\.\d+`') { return $Matches[1] }
    return ''
}

function Get-TaskBlock([string]$TaskId) {
    $block = @()
    $found = $false
    foreach ($line in (Get-AllTodoLines)) {
        if (-not $found -and $line -match ('^- \[.\] `' + [regex]::Escape($TaskId) + '`')) {
            $found = $true; $block += $line; continue
        }
        if ($found) {
            if ($line -match '^- \[.\] `\d+') { break }
            $block += $line
        }
    }
    return ($block -join "`n")
}

function Write-RalphLog([string]$Entry) {
    $dir = Split-Path $LogFile
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    Add-Content -Path $LogFile -Value ("`n## {0}`n`n{1}`n" -f (Get-Date -Format 'yyyy-MM-dd HH:mm'), $Entry)
}

function Set-TaskDone([string]$Id) {
    Get-ChildItem -Path (Join-Path $RepoRoot 'docs') -Filter $TodoGlob -File -ErrorAction SilentlyContinue |
        ForEach-Object {
            $content = Get-Content $_.FullName -Raw
            if ($content -match ('^- \[ \] `' + [regex]::Escape($Id) + '`')) {
                Write-Rwarn "Agent did not mark $Id as done — marking it now."
                $content = $content -replace ('(?m)^- \[ \] `' + [regex]::Escape($Id) + '`'), "- [x] ``$Id``"
                Set-Content $_.FullName $content -NoNewline
                & git add $_.FullName | Out-Null
                $null = & git commit -m "chore(todo): auto-mark $Id done" 2>$null
                $null = & git push origin $BaseBranch 2>$null
            }
        }
}

function Test-VersionLess([string]$A, [string]$B) {
    $pa = $A.Split('.') | ForEach-Object { [int]$_ }
    $pb = $B.Split('.') | ForEach-Object { [int]$_ }
    for ($i = 0; $i -lt 3; $i++) {
        $xa = if ($i -lt $pa.Count) { $pa[$i] } else { 0 }
        $xb = if ($i -lt $pb.Count) { $pb[$i] } else { 0 }
        if ($xa -lt $xb) { return $true }
        if ($xa -gt $xb) { return $false }
    }
    return $false
}

# ── deadline / stop ───────────────────────────────────────────────────────────
function Test-DeadlineReached {
    if ($Deadline -le 0) { return $false }
    return ([DateTimeOffset]::UtcNow.ToUnixTimeSeconds() -ge $Deadline)
}
function Test-StopRequested {
    if ($script:StopRequested) { return $true }
    if (Test-Path $StopSentinel) {
        Write-Rwarn "Stop sentinel found: $StopSentinel — consuming it."
        Remove-Item $StopSentinel -Force
        $script:StopRequested = $true
        return $true
    }
    return $false
}

# ── branch helpers ────────────────────────────────────────────────────────────
function Switch-ToLine([string]$Line) {
    $branch = "release/v$Line"
    $localExists = & git show-ref --verify --quiet "refs/heads/$branch"; if ($LASTEXITCODE -eq 0) { $localExists = $true } else { $localExists = $false }
    $remoteExists = & git ls-remote --exit-code --heads origin $branch 2>$null; if ($LASTEXITCODE -eq 0) { $remoteExists = $true } else { $remoteExists = $false }

    if ($localExists -or $remoteExists) {
        Write-Rlog "Switching to existing $branch"
        & git checkout $branch 2>$null | Out-Null
        & git pull --ff-only origin $branch 2>$null | Out-Null
    }
    else {
        Write-Rlog "Creating ${branch} from $DefaultBranch"
        & git checkout $DefaultBranch 2>$null | Out-Null
        & git pull --ff-only origin $DefaultBranch 2>$null | Out-Null
        & git checkout -b $branch | Out-Null
        & git push -u origin $branch | Out-Null
        Write-Rgood "${branch} created and pushed."
    }
    $script:BaseBranch = $branch
    $script:MinorVersion = $Line
}

# ── semver gates ──────────────────────────────────────────────────────────────
function Test-MajorRelease { return ($MinorVersion -match '\.0$') }

function Start-MajorRc {
    $major = $MinorVersion -replace '\.0$', ''
    $rcVer = "$major.0.0"
    $rcBranch = "rc/v$rcVer-rc.1"
    $rcTag = "v$rcVer-rc.1"

    Write-Rlog "MAJOR RELEASE — line $MinorVersion requires human sign-off."

    & git checkout -b $rcBranch 2>$null | Out-Null
    & git push -u origin $rcBranch 2>$null | Out-Null
    & git tag -a $rcTag -m "Release candidate: $rcTag" 2>$null
    & git push origin $rcTag 2>$null | Out-Null

    Write-RalphLog "MAJOR_RC_READY: $rcBranch + tag $rcTag created. Awaiting human sign-off."
    Write-Rwarn ""
    Write-Rwarn "  RC ready: $rcTag (branch $rcBranch)"
    Write-Rwarn "  Human must review, sign off, then: git checkout $DefaultBranch && git merge --no-ff $rcBranch"
    exit 0
}

# ── line completion review + merge ────────────────────────────────────────────
function Invoke-LineReviewAndMerge {
    if (Test-MajorRelease) { Start-MajorRc }

    Write-Rlog "Line $MinorVersion complete — running release review before merging."

    $maxAttempts = 3
    for ($attempt = 1; $attempt -le $maxAttempts; $attempt++) {
        Write-Rlog "Review attempt $attempt/$maxAttempts"

        $lineStatus = (Get-AllTodoLines | Where-Object { $_ -match ('\`' + [regex]::Escape($MinorVersion) + '\.') } | Select-Object -First 30) -join "`n"
        $changed = (& git diff --name-only "$DefaultBranch...$BaseBranch" 2>$null | Select-Object -First 100) -join "`n"
        $commits = (& git log --oneline "$DefaultBranch...$BaseBranch" 2>$null | Select-Object -First 50) -join "`n"
        $reviewLog = "/tmp/ralph-line-review-$MinorVersion-$attempt.log"

        $prompt = @"
You are performing a release-line completion review for the $RepoName repository.
All tasks in line $MinorVersion are reported complete. Inspect the repo as needed.

Branches: $DefaultBranch vs $BaseBranch

Task status:
$lineStatus

Commits:
$commits

Files changed:
$changed

Checklist — write PASS or FAIL plus one line each:
1. Every $MinorVersion.* task is marked [x].
2. No regressions: checks pass ($FmtCmd; $LintCmd; $TestCmd).
3. docs/memory.md has entries covering architectural choices made in this line.
4. No unrelated scope creep.
5. No security issues introduced.
6. Code quality acceptable.

If every item passes print exactly: PHASE_APPROVED
Otherwise print exactly: PHASE_BLOCKED and list what must be fixed first.
"@
        $reviewOut = Invoke-Agent $ReleaseReviewAgent $prompt | Out-String
        Set-Content -Path $reviewLog -Value $reviewOut

        if ($reviewOut -match 'PHASE_APPROVED') {
            Write-Rgood "Review approved — merging $BaseBranch -> $DefaultBranch, tagging v$MinorVersion.0"
            & git checkout $DefaultBranch 2>$null | Out-Null
            & git pull --ff-only origin $DefaultBranch 2>$null | Out-Null
            & git merge --no-ff $BaseBranch -m "release: merge $BaseBranch into $DefaultBranch — line $MinorVersion complete" | Out-Null
            $newVer = "$MinorVersion.0"
            if (Update-ProjectVersion $newVer) {
                $null = & git commit -m "chore: bump version to $newVer" 2>$null
                Write-Rlog "Project version bumped to $newVer"
            }
            $tagExists = & git show-ref --verify --quiet "refs/tags/v$newVer"; if ($LASTEXITCODE -eq 0) { $tagExists = $true } else { $tagExists = $false }
            if (-not $tagExists) {
                & git tag -a "v$newVer" -m "v${newVer}: release line $MinorVersion complete"
                Write-Rlog "Tagged v$newVer"
            }
            & git push origin $DefaultBranch 2>$null | Out-Null
            & git push origin --tags 2>$null | Out-Null
            Write-RalphLog "PHASE_COMPLETE: $MinorVersion merged to $DefaultBranch; tagged v$newVer."
            return
        }

        Write-Rwarn "Review blocked (attempt $attempt/$maxAttempts)"
        if ($attempt -eq $maxAttempts) {
            Write-RalphLog "PHASE_BLOCKED: $MinorVersion review failed after $maxAttempts attempts."
            exit 1
        }

        $fixPrompt = @"
The release review for line $MinorVersion in $RepoName returned PHASE_BLOCKED.
Fix every issue listed. Minimum changes only. Do NOT commit.
Print exactly: REVIEW_FIXES_DONE when finished.

Review output:
$reviewOut
"@
        $null = Invoke-Agent $ArchitectAgent $fixPrompt | Out-String

        $dirty = (& git status --porcelain) -join ''
        if ($dirty) {
            & git add -A
            $null = & git commit -m "fix: address line $MinorVersion review blockers (attempt $attempt)" 2>$null
        }
    }
}

# ── per-task runner ───────────────────────────────────────────────────────────
function Invoke-SingleTask([string]$TaskId) {
    $branch = "task/$TaskId"
    Write-Rlog "Starting task $TaskId on branch $branch"

    & git checkout $BaseBranch 2>$null | Out-Null
    & git pull --ff-only origin $BaseBranch 2>$null | Out-Null
    & git checkout -b $branch 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-RalphLog "BLOCKED on ${TaskId}: branch ${branch} already exists."
        Write-Rdie "Branch '${branch}' already exists — resolve it manually, then re-run."
    }

    $taskBlock = Get-TaskBlock $TaskId
    $skillText = Get-Content $SkillFile -Raw
    $devAgent = Resolve-DevAgent $taskBlock

    $checksBlock = "Run these checks and make them pass before finishing:"
    foreach ($c in @($FmtCmd, $LintCmd, $TestCmd)) { if ($c) { $checksBlock += "`n  $c" } }

    # Step 1 — plan.
    Write-Rlog 'Step 1/3 — planning'
    $plan = Invoke-Agent $TaskPlanningAgent @"
You are an expert engineer planning a task for the $RepoName repository ($LangName).
Read the skill file and task block, then write a numbered implementation plan. Do NOT write code.

TASK ID: $TaskId

TASK BLOCK:
$taskBlock

SKILL FILE:
$skillText

Produce:
1. Numbered list of files to create/edit (path + one-sentence purpose).
2. Numbered list of tests to write (name + what it proves).
3. Blockers or security concerns, if any.
"@ | Out-String

    # Step 2 — implement.
    Write-Rlog "Step 2/3 — implementing with $devAgent"
    $implOut = Invoke-Agent $devAgent @"
You are Ralph, the autonomous task agent for the $RepoName repository.
Implement task $TaskId in full, following every rule in the skill file below.
Do not commit. Write all files, then verify your work.

$checksBlock
Fix failures until clean. When finished print exactly: IMPLEMENTATION_DONE

TASK ID: $TaskId

TASK BLOCK:
$taskBlock

PLAN:
$plan

SKILL FILE:
$skillText
"@ | Out-String
    ($implOut -split "`n" | Select-Object -Last 15) -join "`n" | Write-Host

    # Step 3 — self-review.
    Write-Rlog 'Step 3/3 — self-review'
    $diff = (& git diff HEAD 2>$null | Out-String)
    if ($diff.Length -gt 30000) { $diff = $diff.Substring(0, 30000) }
    $null = Invoke-Agent $TaskReviewAgent @"
You are reviewing an implementation for the $RepoName repository.
Work through the self-review checklist from the skill file. For each item write PASS
or FAIL plus one line. For any FAIL item, fix it in the code now.
Do not commit. After fixing everything print exactly: REVIEW_DONE

TASK ID: $TaskId

GIT DIFF:
$diff

SKILL FILE (contains the checklist):
$skillText
"@ | Out-String

    # Commit with retry.
    Write-Rlog "Committing $TaskId"
    $commitMsg = (Invoke-Agent $TaskPlanningAgent @"
Write a conventional-commits message for task $TaskId in $RepoName.
First line: '<type>(<scope>): <description, max 50 chars>'.
Blank line, then one short body paragraph (what + why). End with footer: Closes task $TaskId
Output only the message text, no fences.

Task:
$taskBlock
"@ | Out-String).Trim()

    $maxCommitAttempts = 3
    for ($attempt = 1; $attempt -le $maxCommitAttempts; $attempt++) {
        Write-Rlog "Commit attempt $attempt/$maxCommitAttempts"
        & git add -A
        $null = & git commit -m $commitMsg 2>&1
        if ($LASTEXITCODE -eq 0) { Write-Rgood 'Commit succeeded.'; break }

        $commitErr = (& git commit -m $commitMsg 2>&1 | Out-String)
        if ($commitErr -match 'nothing to commit') {
            & git diff --quiet "$BaseBranch...HEAD" 2>$null | Out-Null
            if ($LASTEXITCODE -eq 0) {
                Write-Rwarn "Agent produced no changes — failing task $TaskId."
                Write-RalphLog "FAILED: $TaskId — no changes produced."
                & git checkout $BaseBranch 2>$null | Out-Null
                & git branch -D $branch 2>$null | Out-Null
                return $false
            }
            Write-Rgood 'Working tree clean — commit already exists.'
            break
        }

        if ($attempt -eq $maxCommitAttempts) {
            Write-RalphLog "BLOCKED on ${TaskId}: commit/checks still failing after ${maxCommitAttempts} attempts."
            & git checkout $BaseBranch 2>$null | Out-Null
            & git branch -D $branch 2>$null | Out-Null
            Write-Rdie "Giving up on $TaskId after $maxCommitAttempts attempts."
        }

        $fixPrompt = @"
The commit for task $TaskId in $RepoName failed its checks.
Fix every failure shown below. Change only what is required. Do NOT commit.
Print exactly: FIXES_DONE when done.

Failure output:
$commitErr
"@
        $fixOut = Invoke-Agent $ArchitectAgent $fixPrompt | Out-String
        ($fixOut -split "`n" | Select-Object -Last 15) -join "`n" | Write-Host
    }

    # PR mode: push branch, open PR, stop — a human merges.
    if ($MergeMode -eq 'pr' -and (Get-Command gh -ErrorAction SilentlyContinue)) {
        & git push -u origin $branch 2>$null | Out-Null
        $firstLine = ($commitMsg -split "`n")[0]
        $prUrl = & gh pr create --base $BaseBranch --title $firstLine --body "Closes task $TaskId`n`n$commitMsg" 2>&1 | Out-String
        Write-Rgood "PR opened: $prUrl"
        Write-RalphLog "PR_OPENED: $TaskId — awaiting human merge."
        return $true
    }

    # Merge back into the release line.
    Write-Rlog "Merging $branch into $BaseBranch"
    & git checkout $BaseBranch 2>$null | Out-Null
    & git pull --ff-only origin $BaseBranch 2>$null | Out-Null
    & git merge --no-ff $branch -m "merge: $branch into $BaseBranch" | Out-Null

    Set-TaskDone $TaskId
    & git push origin $BaseBranch 2>$null | Out-Null

    & git branch -d $branch 2>$null | Out-Null
    & git push origin --delete $branch 2>$null | Out-Null

    Write-Rgood "Task $TaskId merged into $BaseBranch."
    Write-RalphLog "DONE: $TaskId merged into $BaseBranch."
    return $true
}

# ── main ──────────────────────────────────────────────────────────────────────

if ($DryRun) {
    Write-Rlog 'Dry run: no changes will be made.'
    Write-Rlog "Repo: $RepoName ($LangName)"
    Write-Rlog "Checks: fmt='$FmtCmd' lint='$LintCmd' test='$TestCmd'"
    if ($SingleTask) {
        $targetBranch = if ($MinorVersion) { $BaseBranch } else { "release/v" + ($SingleTask -replace '\.\d+$', '') }
        Write-Rlog "Would run task $SingleTask on $targetBranch."
    }
    elseif ($MinorVersion) {
        $nt = Get-NextTask $MinorVersion
        if ($nt) { Write-Rlog "Next task on ${BaseBranch}: $nt" }
        else     { Write-Rlog "Line $MinorVersion would go to review/merge." }
    }
    else {
        $nm = Get-NextMinor
        if ($nm) { Write-Rlog "Would switch to release/v$nm and start its next open task." }
        else     { Write-Rlog 'No open tasks found.' }
    }
    exit 0
}

$dirty = (& git status --porcelain) -join ''
if ($dirty) { Write-Rdie 'working tree is dirty — commit or stash before running ralph' }

# Single-task mode.
if ($SingleTask) {
    if (Test-DeadlineReached -or (Test-StopRequested)) {
        Write-Rwarn 'Stop/deadline condition met before task could start.'
        exit 0
    }
    if (-not $MinorVersion) {
        Switch-ToLine ($SingleTask -replace '\.\d+$', '')
    }
    $null = Invoke-SingleTask $SingleTask
    exit 0
}

# Loop mode.
$tasksDone = 0
while ($true) {
    if (Test-DeadlineReached) {
        Write-Rgood "Time limit reached. Tasks completed: $tasksDone."
        Write-RalphLog "Time limit reached. Completed: $tasksDone."
        exit 0
    }
    if (Test-StopRequested) {
        Write-Rgood "Graceful stop. Tasks completed: $tasksDone."
        Write-RalphLog "Graceful stop. Completed: $tasksDone."
        exit 0
    }

    if ($BaseBranch -eq $DefaultBranch) {
        & git pull --ff-only origin $DefaultBranch 2>$null | Out-Null
        $nm = Get-NextMinor
        if (-not $nm) {
            Write-Rgood "All lines complete. Tasks completed: $tasksDone."
            Write-RalphLog "All lines complete. Completed: $tasksDone."
            exit 0
        }
        Switch-ToLine $nm
    }

    $taskId = Get-NextTask $MinorVersion

    if ($taskId -and $Until -and (Test-VersionLess $taskId $Until)) {
        Write-Rgood "Reached -Until boundary ($Until). Stopping."
        exit 0
    }

    if (-not $taskId) {
        Write-Rgood "Line $MinorVersion complete ($tasksDone done this session)."
        if ($MergeMode -eq 'pr') {
            Write-Rgood 'PR mode: open the line-completion PR yourself — ralph stops here.'
            Write-RalphLog "LINE_COMPLETE: $MinorVersion. Human review + merge required (MERGE_MODE=pr)."
            exit 0
        }
        Invoke-LineReviewAndMerge
        $script:BaseBranch = $DefaultBranch
        $script:MinorVersion = ''
        continue
    }

    $ok = Invoke-SingleTask $taskId
    if (-not $ok) {
        Write-Rwarn "Task $taskId failed — logging and moving on."
        Write-RalphLog "FAILED: $taskId."
        & git checkout $BaseBranch 2>$null | Out-Null
        & git branch -D "task/$taskId" 2>$null | Out-Null
    }

    if ($taskId -eq $Until) {
        Write-Rgood "-Until target $Until completed. Stopping."
        exit 0
    }

    if ($MergeMode -eq 'pr') {
        Write-Rgood 'PR mode: stopping after one task/PR. Re-run for the next task.'
        exit 0
    }

    $tasksDone++
    Start-Sleep -Seconds 2
}
