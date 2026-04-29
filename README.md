# Buildmesh

Buildmesh is a high-performance Tauri desktop application for orchestrating multiple AI agents (Claude Code, Gemini, Open Code) across multiple projects concurrently. It provides a tmux-like multiplexer environment designed for developers who need to manage parallel agentic workflows.

## Features

- **Multi-Project Management:** Open and switch between multiple projects seamlessly.
- **Tiled Grid View:** Tile multiple agent terminals in a single view per project. Layouts are saved per-project.
- **Persistent Terminals:** Agents run as durable processes in the background. Switching tabs or projects never interrupts a running agent.
- **Hybrid Runtime Support:** Automatically detects and handles both Windows Native and WSL environments.
- **Git-Ref Checkpoints:** Auto-snapshots your progress using git refs after every prompt.
- **Shortcuts for Speed:**
  - `Alt + 1-9`: Switch sessions.
  - `Alt + G`: Toggle Grid/Single view.
  - `Ctrl + Alt + D`: Toggle Debug Overlay.

## Tech Stack

- **Frontend:** React 19, Zustand 5, xterm.js, Tailwind 4.
- **Backend:** Tauri 2, Rust, portable-pty, SQLite.
- **Runtime:** Windows 10/11 with optional WSL support.

## Development

```bash
# Install dependencies
npm install

# Run in development mode
npm run tauri dev
```
