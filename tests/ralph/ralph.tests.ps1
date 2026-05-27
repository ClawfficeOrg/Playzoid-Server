#Requires -Version 5.1
# ralph.tests.ps1 — Pester unit tests for scripts/ralph.ps1 helper logic.
#
# Tests the regex patterns used by Get-NextTask, Get-CurrentPhase, Get-TaskPhase,
# Get-TaskDefinitionRow, and the argument parser without needing to import the
# full script (which has git startup side effects).
#
# Run with Pester 3, 4, or 5:
#   Invoke-Pester tests\ralph\ralph.tests.ps1
#
# Pester version compatibility notes:
#   Pester 3.x  — uses "Should Be"  / "Should Not Be" (no dash prefix)
#   Pester 4+   — adds "Should -Be" / "Should -Not -Be" forms
#   Pester 5+   — adds BeforeAll / AfterAll blocks
#   This file targets Pester 3.x syntax; both 4.x and 5.x also accept it.
#
# PowerShell 5.1 vs 7+ compatibility:
#   [regex]::Escape()         — available in both
#   -split "`n"               — LF-only; safe because Windows Get-Content
#                               strips CR by default
#   [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
#                             — requires .NET 4.7.2+; present on all
#                               Windows 10+ PS 5.1 installs

# ── helper functions mirroring ralph.ps1 internals ────────────────────────────

function Invoke-GetNextTask
{
    param([string]$TodoContent, [string]$Phase)
    $esc  = [regex]::Escape($Phase)
    $pat  = "^\| *${esc}-\d+ *\| *⏳"
    $line = $TodoContent -split "`n" |
        Where-Object { $_ -match $pat } |
        Select-Object -First 1
    if ($line -and $line -match "^\| *(\d+\.\d+-\d+) *\|")
    { $Matches[1] 
    } else
    { "" 
    }
}

function Invoke-GetCurrentPhase
{
    param([string]$TodoContent)
    $line = $TodoContent -split "`n" |
        Where-Object { $_ -match "^\| *\d+\.\d+-\d+ *\| *⏳" } |
        Select-Object -First 1
    if ($line -and $line -match "^\| *(\d+\.\d+)-\d+")
    { $Matches[1] 
    } else
    { "" 
    }
}

function Invoke-GetTaskPhase
{
    param([string]$TaskId)
    $TaskId -replace '-\d+$', ''
}

function Invoke-GetTaskDefinitionRow
{
    param([string]$TodoContent, [string]$TaskId)
    $esc = [regex]::Escape($TaskId)
    $TodoContent -split "`n" |
        Where-Object { $_ -match "^\| *${esc} *\| *[A-Za-z``]" } |
        Select-Object -First 1
}

# ── mock docs/TODO.md fixture ─────────────────────────────────────────────────
# Mirrors the real TODO.md table format used by Playzoid-Server.

$script:MockTodo = @"
## Phase 0.2.0 — Authentication & Player Management

**Tasks:**

| # | Task | Complexity | Agent |
|---|------|------------|-------|
| 0.2-1 | Implement ``POST /auth/login`` — validate credentials, issue JWT | medium | sonnet |
| 0.2-2 | Implement JWT middleware for protected routes | medium | sonnet |
| 0.2-3 | Implement ``POST /auth/register`` — create new player account | medium | sonnet |
| 0.2-7 | Add ``parent_account_id`` to player schema + migration | small | sonnet |

**Phase 0.2 status:**

| #      | Status           | Evidence |
|--------|------------------|----------|
| 0.2-1  | ⏳ pending       | depends on auth helpers |
| 0.2-2  | ⏳ pending       | depends on 0.2-1 |
| 0.2-3  | ⏳ pending       | depends on 0.2-1 |
| 0.2-7  | ✅ verified      | already in migration |

## Phase 0.3.0 — Leaderboards & WebSocket

**Tasks:**

| # | Task | Complexity | Agent |
|---|------|------------|-------|
| 0.3-1 | DB migration: leaderboards table | small | nemotron |

**Phase 0.3 status:**

| #     | Status      | Evidence |
|-------|-------------|----------|
| 0.3-1 | ⏳ pending  | not started |
"@

$script:MockTodoPartial = @"
| 0.2-1 | ⏳ pending       | task one |
| 0.2-2 | 🟡 partial       | half done |
| 0.2-3 | ✅ done          | merged PR #5 |
| 0.2-4 | 📬 PR #8         | in review |
"@

$script:MockTodoAllDone = @"
| 0.2-1 | ✅ done     | merged |
| 0.2-2 | ✅ done     | merged |
| 0.2-3 | ✅ verified | already existed |
"@

# ── Get-NextTask tests ─────────────────────────────────────────────────────────

Describe "Get-NextTask" {
    It "returns the first pending task for the given phase" {
        Invoke-GetNextTask $script:MockTodo "0.2" | Should Be "0.2-1"
    }

    It "skips tasks that are done (✅)" {
        Invoke-GetNextTask $script:MockTodo "0.2" | Should Not Be "0.2-7"
    }

    It "does NOT return tasks from a different phase" {
        Invoke-GetNextTask $script:MockTodo "0.2" | Should Not Be "0.3-1"
    }

    It "avoids partial-phase match (0.2 should not match 0.20)" {
        $futureTodo = "| 0.20-1 | ⏳ pending | future task |"
        Invoke-GetNextTask $futureTodo "0.2" | Should BeNullOrEmpty
    }

    It "returns empty string when all tasks in the phase are done" {
        Invoke-GetNextTask $script:MockTodoAllDone "0.2" | Should BeNullOrEmpty
    }

    It "returns empty string for a phase with no tasks at all" {
        Invoke-GetNextTask $script:MockTodo "9.9" | Should BeNullOrEmpty
    }

    It "skips tasks marked 📬 PR (in review)" {
        # Task 0.2-4 is 📬 PR #8 — should not be returned
        $result = Invoke-GetNextTask $script:MockTodoPartial "0.2"
        $result | Should Be "0.2-1"
        $result | Should Not Be "0.2-4"
    }

    It "returns only ⏳ pending, not 🟡 partial" {
        # 🟡 partial tasks are tracked separately; Get-NextTask only targets ⏳
        $allPartial = "| 0.2-1 | 🟡 partial | half done |"
        Invoke-GetNextTask $allPartial "0.2" | Should BeNullOrEmpty
    }
}

# ── Get-CurrentPhase tests ─────────────────────────────────────────────────────

Describe "Get-CurrentPhase" {
    It "returns the phase of the first pending task across all phases" {
        Invoke-GetCurrentPhase $script:MockTodo | Should Be "0.2"
    }

    It "returns the next phase when the current phase is all done" {
        $phase02done = @"
| 0.2-1 | ✅ done | merged |
| 0.2-2 | ✅ done | merged |
| 0.3-1 | ⏳ pending | not started |
"@
        Invoke-GetCurrentPhase $phase02done | Should Be "0.3"
    }

    It "returns empty string when no open tasks remain anywhere" {
        Invoke-GetCurrentPhase $script:MockTodoAllDone | Should BeNullOrEmpty
    }

    It "correctly strips the task suffix for multi-digit minor versions" {
        $futureTodo = "| 1.10-3 | ⏳ pending | future task |"
        Invoke-GetCurrentPhase $futureTodo | Should Be "1.10"
    }

    It "correctly handles phase 1.0 (dot-zero minor)" {
        $prodTodo = "| 1.0-1 | ⏳ pending | analytics |"
        Invoke-GetCurrentPhase $prodTodo | Should Be "1.0"
    }
}

# ── Get-TaskPhase tests ────────────────────────────────────────────────────────

Describe "Get-TaskPhase" {
    It "extracts 0.2 from 0.2-1"  { Invoke-GetTaskPhase "0.2-1"  | Should Be "0.2"  }
    It "extracts 0.3 from 0.3-15" { Invoke-GetTaskPhase "0.3-15" | Should Be "0.3"  }
    It "extracts 1.0 from 1.0-14" { Invoke-GetTaskPhase "1.0-14" | Should Be "1.0"  }
    It "extracts 1.10 from 1.10-3" { Invoke-GetTaskPhase "1.10-3" | Should Be "1.10" }
}

# ── Get-TaskDefinitionRow tests ────────────────────────────────────────────────

Describe "Get-TaskDefinitionRow" {
    It "returns the 4-column task definition row, not the status row" {
        $row = Invoke-GetTaskDefinitionRow $script:MockTodo "0.2-1"
        $row | Should Not BeNullOrEmpty
        $row | Should Match "auth/login"
        $row | Should Not Match "⏳"
        $row | Should Not Match "pending"
    }

    It "returns empty for a task id not in the definition table" {
        Invoke-GetTaskDefinitionRow $script:MockTodo "9.9-1" | Should BeNullOrEmpty
    }

    It "does not confuse 0.2-1 with 0.2-10 or 0.2-11" {
        $extended = $script:MockTodo + "`n| 0.2-10 | Some other task | small | nemotron |"
        $row = Invoke-GetTaskDefinitionRow $extended "0.2-1"
        $row | Should Match "0\.2-1 "
        $row | Should Not Match "0\.2-10"
    }
}

# ── Argument parsing tests ─────────────────────────────────────────────────────

Describe "Argument parsing" {
    function Invoke-ParseArgs
    {
        param([string[]]$ArgList)
        $durationSecs = 0
        $singleTask   = ""
        $loopMode     = $false
        $errors       = @()
        foreach ($a in $ArgList)
        {
            if      ($a -match '^--minutes=(\d+)$')
            { $durationSecs += [int]$Matches[1] * 60 
            } elseif  ($a -match '^--hours=(\d+)$')
            { $durationSecs += [int]$Matches[1] * 3600 
            } elseif  ($a -eq '--loop')
            { $loopMode = $true 
            } elseif  ($a -match '^-')
            { $errors += "unknown flag: $a" 
            } else
            {
                if ($singleTask -ne "")
                { $errors += "too many positional args" 
                }
                $singleTask = $a
            }
        }
        [PSCustomObject]@{
            DurationSecs = $durationSecs
            SingleTask   = $singleTask
            LoopMode     = $loopMode
            Errors       = $errors
        }
    }

    It "parses --minutes=30 as 1800 seconds" {
        (Invoke-ParseArgs @("--minutes=30")).DurationSecs | Should Be 1800
    }

    It "parses --hours=2 as 7200 seconds" {
        (Invoke-ParseArgs @("--hours=2")).DurationSecs | Should Be 7200
    }

    It "accumulates --hours=1 --minutes=30 as 5400 seconds" {
        (Invoke-ParseArgs @("--hours=1", "--minutes=30")).DurationSecs | Should Be 5400
    }

    It "captures the positional task id" {
        (Invoke-ParseArgs @("0.2-1")).SingleTask | Should Be "0.2-1"
    }

    It "captures a task id alongside time flags" {
        $r = Invoke-ParseArgs @("--minutes=45", "0.2-4")
        $r.DurationSecs | Should Be 2700
        $r.SingleTask   | Should Be "0.2-4"
    }

    It "sets loop mode when --loop is passed" {
        (Invoke-ParseArgs @("--loop")).LoopMode | Should Be $true
    }

    It "loop mode is false by default" {
        (Invoke-ParseArgs @("0.2-1")).LoopMode | Should Be $false
    }

    It "reports error on unknown flags" {
        (Invoke-ParseArgs @("--foo=bar")).Errors.Count | Should BeGreaterThan 0
    }

    It "reports error on two positional arguments" {
        (Invoke-ParseArgs @("0.2-1", "0.2-2")).Errors.Count | Should BeGreaterThan 0
    }
}

# ── Task id format validation tests ───────────────────────────────────────────

Describe "Task id format" {
    function Test-TaskIdFormat
    { param([string]$Id) $Id -match '^\d+\.\d+-\d+$' 
    }

    It "accepts 0.2-1"   { Test-TaskIdFormat "0.2-1"   | Should Be $true }
    It "accepts 0.3-15"  { Test-TaskIdFormat "0.3-15"  | Should Be $true }
    It "accepts 1.0-14"  { Test-TaskIdFormat "1.0-14"  | Should Be $true }
    It "accepts 1.10-3"  { Test-TaskIdFormat "1.10-3"  | Should Be $true }
    It "rejects 0.2.1"   { Test-TaskIdFormat "0.2.1"   | Should Be $false }
    It "rejects 1.2.3"   { Test-TaskIdFormat "1.2.3"   | Should Be $false }
    It "rejects foo-bar" { Test-TaskIdFormat "foo-bar"  | Should Be $false }
    It "rejects empty"   { Test-TaskIdFormat ""         | Should Be $false }
}

# ── Syntax check ───────────────────────────────────────────────────────────────

Describe "ralph.ps1 syntax" {
    It "parses without a syntax error" {
        $scriptPath = Join-Path $PSScriptRoot "..\..\scripts\ralph.ps1"
        $err = $null
        try
        {
            $null = [System.Management.Automation.ScriptBlock]::Create(
                (Get-Content $scriptPath -Raw)
            )
        } catch
        {
            $err = $_.Exception.Message
        }
        $err | Should BeNullOrEmpty
    }
}
