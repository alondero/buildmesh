# Diagnostic: show the LAST 8 log lines + any buildmesh-dev process status.
$logPath = Join-Path $env:APPDATA 'com.alond.buildmesh.dev\logs\buildmesh.log'
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
