# Diagnostic: show the LAST 8 log lines + any buildmesh-dev process status.
#
# Reads the profile this worktree's last launch recorded, because a linked
# worktree runs its own dev instance on its own ports (run-dev.ps1 writes
# .tmp/dev-instance.json); without it this would report the canonical dev
# profile's log, which says nothing about the instance actually running here.
# Falls back to the canonical dev profile when no launch has happened yet.
$recordPath = Join-Path $PSScriptRoot '..\.tmp\dev-instance.json'
$profileDir = $null
if (Test-Path $recordPath) {
    try {
        # ReadAllText, not Get-Content: a launch writing the record at this
        # moment would abort the diagnostic under $ErrorActionPreference = Stop.
        $profileDir = ([System.IO.File]::ReadAllText($recordPath) | ConvertFrom-Json).profileDir
    } catch {
        Write-Output "Ignoring an unreadable instance record at $recordPath"
    }
}
if (-not $profileDir) {
    $profileDir = Join-Path $env:APPDATA 'com.alond.buildmesh.dev'
}
$logPath = Join-Path $profileDir 'logs\buildmesh.log'
Write-Output "=== DEV PROFILE: $profileDir ==="
if (Test-Path $logPath) {
    Write-Output "=== LOG TAIL (last 8 lines) ==="
    Get-Content $logPath -Tail 8 | Write-Output
} else {
    Write-Output "no log file"
}
$proc = Get-Process buildmesh-dev -ErrorAction SilentlyContinue
if ($proc) {
    Write-Output "=== PROCESS ALIVE ==="
    $proc | Select-Object Id, StartTime | Format-List | Out-String | Write-Output
} else {
    Write-Output "=== NO buildmesh-dev process running ==="
}