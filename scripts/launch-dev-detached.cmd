@echo off
REM Launches the already-built buildmesh-dev binary fully detached (so the
REM parent shell exiting doesn't cascade-kill the GUI). Skips the rebuild that
REM run-dev.ps1 would do. Resolves paths from the script's own directory so
REM it works from any checkout or worktree.
setlocal
set "BINARY=%~dp0..\src-tauri\target\release-dev\release\buildmesh-dev.exe"
if not exist "%BINARY%" (
    echo ERROR: %BINARY% not found — run scripts\run-dev.ps1 first
    exit /b 1
)
start "" "%BINARY%"
