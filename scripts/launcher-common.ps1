# Shared helpers for the Windows launchers (run.ps1, run-dev.ps1).
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
