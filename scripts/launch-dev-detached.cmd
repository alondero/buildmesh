@echo off
REM Launches the already-built buildmesh-dev binary fully detached (so the
REM parent shell exiting doesn't cascade-kill the GUI). Skips the rebuild that
REM run-dev.ps1 would do. Resolves paths from the script's own directory so
REM it works from any checkout or worktree.
REM
REM Delegates to run-dev.ps1 -SkipBuild rather than starting the binary here:
REM the launcher owns the instance identity (this worktree's profile dir and
REM port offset) and the stop-only-what-I-launched rule, and duplicating either
REM in batch is how a second worktree ends up on the first one's ports.
setlocal
if not exist "%~dp0..\src-tauri\target\release-dev\release\buildmesh-dev.exe" (
    echo ERROR: %~dp0..\src-tauri\target\release-dev\release\buildmesh-dev.exe not found — run scripts\run-dev.ps1 first
    exit /b 1
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run-dev.ps1" -SkipBuild -Detach
exit /b %ERRORLEVEL%