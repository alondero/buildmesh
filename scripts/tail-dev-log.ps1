$logPath = Join-Path $env:APPDATA "com.alond.buildmesh.dev\logs\buildmesh.log"
Write-Output "Tailing: $logPath"
if (-not (Test-Path $logPath)) {
    Write-Output "ERROR: log file not found at $logPath"
    exit 1
}
Get-Content $logPath -Wait -Tail 0
