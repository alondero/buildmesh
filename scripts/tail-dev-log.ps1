. (Join-Path $PSScriptRoot 'launcher-common.ps1')
# The profile this worktree's last launch recorded: a linked worktree runs its
# own dev instance (issue #2345), and tailing the canonical dev profile would
# show another worktree's or the main checkout's build.
$logPath = Join-Path (Resolve-DevProfileDir) 'logs\buildmesh.log'
Write-Output "Tailing: $logPath"
if (-not (Test-Path $logPath)) {
    Write-Output "ERROR: log file not found at $logPath"
    exit 1
}
Get-Content $logPath -Wait -Tail 0