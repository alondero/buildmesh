# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Buildmesh is a Tauri desktop application for orchestrating AI agents (Claude Code, Gemini, Open Code) across multiple projects concurrently. It provides a multiplexer-style environment with persistent terminals and hybrid Windows/WSL support.

## Architecture

### High-Level Design

Single-window desktop app with a **sidebar** for navigation and a **Session View** with a tab bar.
- **Persistent Terminal Registry:** `xterm.js` instances are stored in a global registry (`Terminal.tsx`), ensuring colors and context are preserved during session switches.
- **Hybrid Path Mapping:** Linux paths in WSL are mapped to Windows UNC paths (`\\wsl$\...`) for host-side file tree and watcher operations.
- **PTY Management:** Simplified backend PTY lifecycle in `agent.rs`. Agents are spawned as durable processes.

### Key Data Types

- **Project:** Top-level folder on disk.
- **Session:** An isolated agent instance. Persists `cli_session_id` for robust `--resume` support.
- **Layout:** Supports `single` and `grid` (split-pane) views.

## Technical Stack

- **Frontend:** React 19, Zustand 5, xterm.js, Tailwind 4.
- **Backend:** Tauri 2, Rust, portable-pty, rusqlite, regex.
- **Environment:** Automated detection and path mapping between Windows and WSL.

## Database Schema (v3)

- **projects:** id, name, path, created_at.
- **sessions:** id, project_id, name, path, branch, env, provider, status, cli_session_id, created_at.
- **checkpoints:** id, session_id, git_ref, turn_index, message, created_at.

## Guidelines

- **Terminal Persistence:** Never dispose of a terminal instance unless the session is explicitly archived/deleted.
- **Path Handling:** Use `env::to_host_path` when accessing the file system from the backend to ensure compatibility with WSL sessions.
- **Agent Spawning:** On Windows, spawn `cwrap` via `cmd.exe /c` to ensure ConPTY correctly handles ANSI sequences.
- **Session ID Capture:** The backend PTY reader thread automatically captures `session-id` patterns from agent output to update the database for future resumes.
