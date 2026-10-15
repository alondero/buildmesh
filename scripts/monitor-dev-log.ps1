# Wait for the dev log file to appear, then tail it filtered for notable lines.
. (Join-Path $PSScriptRoot 'launcher-common.ps1')
# The profile this worktree's last launch recorded: a linked worktree runs its
# own dev instance (issue #2345), and monitoring the canonical dev profile would
# report another worktree's or the main checkout's launch as this one's.
$logPath = Join-Path (Resolve-DevProfileDir) 'logs\buildmesh.log'
$pattern = 'ERROR|error|failed|Failed|WARN|warn'

$logDir = Split-Path -Parent $logPath
if (-not (Test-Path $logDir)) {
    New-Item -ItemType Directory -Path $logDir -Force | Out-Null
}

# Poll until the log file exists. Buildmesh's first action is to create the
# log file via tracing-appender; if it doesn't appear within ~30s something
# is wrong with the launch.
$deadline = (Get-Date).AddSeconds(30)
while (-not (Test-Path $logPath)) {
    if ((Get-Date) -gt $deadline) {
        Write-Output "monitor-dev-log: log file not created within 30s at $logPath"
        exit 0
    }
    Start-Sleep -Milliseconds 500
}

# Stream new lines as they arrive, filter through Select-String.
Get-Content -Path $logPath -Wait -Tail 0 | Select-String -Pattern $pattern
