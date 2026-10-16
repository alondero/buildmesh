# Diagnostic: show the LAST 8 log lines + any buildmesh-dev process status.
. (Join-Path $PSScriptRoot 'launcher-common.ps1')

# The profile this worktree's dev instance writes to, because a linked worktree
# runs its own instance on its own ports; the canonical dev profile's log says
# nothing about the instance running here.
$ProfileDir = Resolve-DevProfileDir
$logPath = Join-Path $ProfileDir 'logs\buildmesh.log'
Write-Output "=== DEV PROFILE: $ProfileDir ==="
if (Test-Path $logPath) {
    Write-Output "=== LOG TAIL (last 8 lines) ==="
    Get-Content $logPath -Tail 8 | Write-Output
} else {
    Write-Output "no log file"
}

# Report THIS worktree's instance, not merely a process with the right name:
# every worktree's dev build is called buildmesh-dev, so an unqualified
# `Get-Process buildmesh-dev` answers "yes, running" for a worktree whose own
# instance is not up, which is exactly the question this diagnostic exists to
# answer. Owned = the executable this worktree built; foreign = another one.
# The path is built with GetFullPath, not Resolve-Path: this script has to run
# BEFORE the first dev build, when target\release-dev does not exist, and
# Resolve-Path queries the filesystem and throws ItemNotFoundException for a
# missing directory. GetFullPath only normalizes the string. The comparison is
# a path comparison, so the file need not exist on disk - a running process
# reports the path it was launched from, which is the path this worktree would
# have used. Anchored to the script's own location so it does not depend on the
# caller's working directory.
$RepoRoot = Join-Path $PSScriptRoot '..'
$Binary = [System.IO.Path]::GetFullPath(
    (Join-Path $RepoRoot 'src-tauri\target\release-dev\release\buildmesh-dev.exe'))
$owned = @()
$foreign = @()
foreach ($proc in @(Get-Process buildmesh-dev -ErrorAction SilentlyContinue)) {
    $path = $null
    try { $path = $proc.Path } catch { }
    if ($path -and [System.IO.Path]::GetFullPath($path) -eq [System.IO.Path]::GetFullPath($Binary)) {
        $owned += $proc
    } else {
        $foreign += $proc
    }
}
if ($owned.Count -gt 0) {
    Write-Output "=== THIS WORKTREE'S INSTANCE ALIVE ==="
    $owned | Select-Object Id, StartTime | Format-List | Out-String | Write-Output
} else {
    Write-Output "=== NO buildmesh-dev from this worktree running ==="
}
if ($foreign.Count -gt 0) {
    # Not a verdict about this worktree, and deliberately not killed: another
    # session's instance is none of this diagnostic's business.
    Write-Output "=== OTHER WORKTREES' INSTANCES RUNNING (left alone) ==="
    $foreign | ForEach-Object { Write-Output "PID $($_.Id)" }
}