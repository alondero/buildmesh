$ErrorActionPreference = "Stop"

Set-Location "$PSScriptRoot\.."

$Binary = "src-tauri\target\release\buildmesh.exe"
$LogPath = "$env:APPDATA\com.alond.buildmesh\logs\buildmesh.log"

# 1. Kill existing instances
$existing = Get-Process -Name 'buildmesh' -ErrorAction SilentlyContinue
if ($existing) {
    Write-Output "Stopping existing buildmesh..."
    $existing | Stop-Process -Force
    Start-Sleep -Milliseconds 1000
}

# 2. Build (frontend + Rust)
Write-Output "Building..."
npm run tauri build
if ($LASTEXITCODE -ne 0) {
    Write-Output "ERROR: Build failed"
    exit 1
}

# 3. Verify binary exists
if (-not (Test-Path $Binary)) {
    Write-Output "ERROR: Build failed - $Binary not found"
    exit 1
}

# 4. Record log position
$BeforeLines = 0
if (Test-Path $LogPath) {
    $BeforeLines = (Get-Content $LogPath).Count
}

# 5. Launch raw binary
$proc = Start-Process $Binary -PassThru
Write-Output "Launched PID: $($proc.Id)"

# 6. Verify via log
Start-Sleep -Seconds 3
if (Test-Path $LogPath) {
    $AllLines = Get-Content $LogPath
    if ($AllLines.Count -gt $BeforeLines) {
        $NewLines = $AllLines[$BeforeLines..($AllLines.Count - 1)]
        $started = $NewLines | Where-Object { $_ -match "started|ready" }
        if ($started) {
            Write-Output "OK - Buildmesh running"
            exit 0
        }
    }
}

# Fallback: check process is alive
if (-not $proc.HasExited) {
    Write-Output "OK - Process alive (no log confirmation)"
    exit 0
}

Write-Output "ERROR: Buildmesh failed to start"
exit 1
