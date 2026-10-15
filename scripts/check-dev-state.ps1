# Diagnostic: show the LAST 8 log lines + any buildmesh-dev process status.
. (Join-Path $PSScriptRoot 'launcher-common.ps1')

# The profile this worktree's last launch recorded, because a linked worktree
# runs its own dev instance on its own ports; the canonical dev profile's log
# says nothing about the instance running here.
$profileDir = Resolve-DevProfileDir
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