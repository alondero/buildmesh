# Shared helpers for the Windows launchers (run.ps1, run-dev.ps1) and the dev
# log helpers that have to look at the same profile.
#
# Dot-sourced, not executed:
#     . (Join-Path $PSScriptRoot "launcher-common.ps1")
#
# Issue #2043 - both launchers run under $ErrorActionPreference = "Stop", which
# is script-wide: any error record aborts the script and leaves powershell.exe
# reporting 1 for a launch that is already up and healthy. Reads the launch
# verdict depends on therefore go through Read-LogFile instead of Get-Content,
# which retries a transient sharing violation (the app holds buildmesh.log open
# while writing) and reports an unreadable file instead of aborting.
#
# The result object is deliberately explicit rather than a bare line array,
# because the delta checks have to distinguish "read it and it had N lines" from
# "could not read it at all". Collapsing the second case to zero lines is the
# bug this shape prevents: a failed post-launch read would make 0 -gt N false
# and silently mask a real panic (issue #158), and a failed baseline would make
# every line look new and false-panic a healthy launch (issue #2043).

function Read-LogFile {
    <#
    .SYNOPSIS
        Reads a launcher log file, retrying transient sharing violations.

    .DESCRIPTION
        Returns an object with:
          Readable - $true when the file was read (or does not exist yet, which
                     is legitimately zero lines), $false when it could not be.
          Lines    - the file's lines; @() whenever Readable is $false.

        Never throws, so a caller cannot turn a read failure into a verdict.
        Callers MUST check .Readable before comparing line counts.

    .PARAMETER Path
        Path to the log file.

    .PARAMETER Attempts
        Read attempts before giving up. A sharing violation clears once the
        writer releases the handle, so one retry usually suffices; the app
        holds buildmesh.log for its whole life, so that file legitimately
        exhausts them and the caller falls back to the process-alive verdict.

    .PARAMETER RetryDelayMs
        Delay between attempts.
    #>
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [int]$Attempts = 3,
        [int]$RetryDelayMs = 200
    )

    # A missing file is not a read failure: the app may not have created it yet.
    if (-not (Test-Path -LiteralPath $Path)) {
        return [pscustomobject]@{ Readable = $true; Lines = @() }
    }

    for ($attempt = 1; $attempt -le $Attempts; $attempt++) {
        try {
            return [pscustomobject]@{ Readable = $true; Lines = @(Get-Content -LiteralPath $Path) }
        } catch {
            if ($attempt -lt $Attempts) {
                Start-Sleep -Milliseconds $RetryDelayMs
            }
        }
    }

    # Warning stream, not Write-Host: it must not land on the success stream
    # that the launchers' stdout contract is parsed from.
    Write-Warning "could not read ${Path} after ${Attempts} attempts; treating it as no evidence"
    return [pscustomobject]@{ Readable = $false; Lines = @() }
}

function Format-Count {
    <#
    .SYNOPSIS
        Renders a Read-LogFile result for the launcher's stdout contract.

    .DESCRIPTION
        Prints the line count, or "unreadable". The launchers echo pre-launch
        counts so /verify can slice the post-launch file; printing a bare 0 for
        an unreadable baseline would tell it to treat every subsequent line as
        new, so the state has to be visible in the output instead.
    #>
    param([Parameter(Mandatory = $true)]$Result)
    if ($Result.Readable) { return $Result.Lines.Count }
    return "unreadable"
}

function Compare-LogGrowth {
    <#
    .SYNOPSIS
        Decides whether a log grew since a captured baseline.

    .DESCRIPTION
        Returns an object with:
          Checked - $false when the comparison could not be made because either
                    read was unreadable. The caller must skip its verdict on an
                    unchecked result rather than treat it as "no growth".
          Grew    - $true only when both reads succeeded AND the file has more
                    lines than the baseline.
          Lines   - the current lines when Checked is $true, else @().

        Both failure directions matter, and they pull in opposite directions:
          - baseline unreadable, current read fine -> every line looks new, so a
            naive count comparison false-panics a healthy launch (#2043);
          - current read unreadable                -> 0 is never greater than N,
            so a naive comparison reports "no panic" after the panic hook has
            already written its entry, masking a real panic-only crash (#158).
        An unchecked result is therefore never a clean bill of health, and never
        a failure either: the caller falls through to its next evidence source.
    #>
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)]$Before
    )

    if (-not $Before.Readable) {
        Write-Warning "skipping growth check for ${Path}: pre-launch baseline was unreadable"
        return [pscustomobject]@{ Checked = $false; Grew = $false; Lines = @() }
    }

    $after = Read-LogFile $Path
    if (-not $after.Readable) {
        Write-Warning "skipping growth check for ${Path}: post-launch read failed"
        return [pscustomobject]@{ Checked = $false; Grew = $false; Lines = @() }
    }

    return [pscustomobject]@{
        Checked = $true
        Grew    = ($after.Lines.Count -gt $Before.Lines.Count)
        Lines   = $after.Lines
    }
}

function Get-WorktreeSlug {
    <#
    .SYNOPSIS
        The per-worktree identity suffix for a dev instance, or $null.

    .DESCRIPTION
        Returns $null for the main checkout. A linked worktree is the one whose
        `.git` is a FILE rather than a directory - `git worktree add` writes
        `gitdir: ...` into it. Anything else (a plain checkout, an exported
        copy, a test fixture) is the main checkout.

        The leaf alone is not unique: `.worktrees/<name>` and
        `.claude/worktrees/<name>` are both real worktree layouts, so the leaf
        is paired with a hash of the full path, lower-cased so two spellings of
        one path resolve to one profile directory.

        Lives here rather than in run-dev.ps1 so the launcher and the log
        helpers derive the same directory for the same worktree.
    #>
    param([Parameter(Mandatory = $true)][string]$RepoRoot)

    if (-not (Test-Path -LiteralPath (Join-Path $RepoRoot '.git') -PathType Leaf)) {
        return $null
    }
    $leaf = Split-Path $RepoRoot -Leaf
    $safe = $leaf -replace '[^A-Za-z0-9._-]', '-'
    if ([string]::IsNullOrWhiteSpace($safe)) { $safe = 'worktree' }
    if ($safe.Length -gt 48) { $safe = $safe.Substring(0, 48) }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha.ComputeHash(
            [System.Text.Encoding]::UTF8.GetBytes($RepoRoot.ToLowerInvariant())
        )
    } finally {
        $sha.Dispose()
    }
    $hex = -join ($digest | ForEach-Object { $_.ToString('x2') })
    return "$safe-$($hex.Substring(0, 8))"
}

function Resolve-DevProfileDir {
    <#
    .SYNOPSIS
        The app-data profile this worktree's dev instance writes its logs to.

    .DESCRIPTION
        A linked worktree runs its own dev instance: run-dev.ps1 gives it its
        own profile dir and port offset (issue #2345), so a helper that reads
        the dev log has to read THAT instance's log. Reading the canonical dev
        profile instead reports another worktree's, or the main checkout's,
        build - the wrong window's lines presented as this one's.

        Resolution order, most authoritative first:
          1. .tmp\dev-instance.json - the profile the last launch opened.
             Absent in two cases that matter: a helper that runs BEFORE the
             first launch, and the build phase of a re-launch (the launcher
             consumes the record before it builds).
          2. $env:BUILDMESH_PROFILE_DIR - a profile the caller pinned.
          3. The worktree's own directory, derived from the slug. Deterministic,
             so it is correct before anything has been launched here.
          4. The canonical dev profile - the main checkout, which uses it.

        Never throws: a damaged record resolves to the next option instead of
        aborting a diagnostic whose whole job is to report state.

    .PARAMETER RepoRoot
        The worktree root. Defaults to the parent of this script's directory,
        which is the layout every caller in scripts\ sits in.
    #>
    param([string]$RepoRoot)

    if (-not $RepoRoot) { $RepoRoot = Join-Path $PSScriptRoot '..' }
    $recordPath = Join-Path $RepoRoot '.tmp\dev-instance.json'
    if (Test-Path -LiteralPath $recordPath) {
        try {
            # ReadAllText, not Get-Content: a launcher rewriting the record at
            # this moment would raise a sharing violation and abort the caller.
            $record = [System.IO.File]::ReadAllText($recordPath) | ConvertFrom-Json
            if ($record.profileDir) { return $record.profileDir }
        } catch {
            Write-Warning "ignoring an unreadable instance record at ${recordPath}"
        }
    }
    if ($env:BUILDMESH_PROFILE_DIR) { return $env:BUILDMESH_PROFILE_DIR }
    $slug = Get-WorktreeSlug -RepoRoot $RepoRoot
    if ($slug) {
        return (Join-Path (Join-Path $env:APPDATA 'com.alond.buildmesh.dev\worktrees') $slug)
    }
    return (Join-Path $env:APPDATA 'com.alond.buildmesh.dev')
}
