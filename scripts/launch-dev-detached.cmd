@echo off
REM Launches the already-built buildmesh-dev binary fully detached (so the
REM parent shell exiting doesn't cascade-kill the GUI). Skips the rebuild that
REM run-dev.ps1 would do.
start "" "X:\src\buildmesh\.claude\worktrees\fair-raw-wind\src-tauri\target\release\buildmesh-dev.exe"
