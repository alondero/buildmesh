# Buildmesh Architecture Learnings

This document captures key technical insights and architectural decisions made during the Buildmesh development process, specifically addressing terminal stability and cross-environment support.

## 1. Terminal Persistence & Rendering

### The Problem
Initial implementations suffered from "terminal blanking" or loss of color/context when switching between session tabs. This was caused by React's component lifecycle unmounting the `xterm.js` instance and attempting to reconstruct it from plain text.

### The Solution: Hidden Terminal Stack
Instead of re-parenting or reconstructing terminals, we implemented a **Persistent Terminal Stack**:
- **Global Manager:** A `TerminalManager` (singleton) maintains a `Map<number, TerminalInstance>` that survives any React unmounting.
- **Hidden Containers:** The `TerminalStack` component renders a `TerminalContainer` for *every* session that has been opened.
- **CSS Toggling:** Sessions are hidden/shown using the CSS `hidden` attribute (display: none) rather than being removed from the DOM.
- **Re-fitting:** Terminals require an explicit `.fit()` call when transitioning from `display: none` to `block`. We used `requestAnimationFrame` and `setTimeout` to ensure the DOM had settled before fitting.

## 2. Hybrid Environment Path Mapping (Windows/WSL)

### The Problem
The Tauri backend runs as a native Windows process, but agents often run inside WSL. This caused "File Not Found" errors when the Windows backend tried to list directories or watch files using Linux-style paths (e.g., `/home/user/project`).

### The Solution: UNC Mapping
We implemented a robust path translation layer in `env/mod.rs`:
- **Inbound (Guest -> Host):** `to_host_path` detects Linux paths and maps them to Windows UNC paths (e.g., `\\wsl$\Ubuntu\home\...`).
- **Outbound (Host -> Guest):** The file tree component maps these back to their original "internal" form before sending them to the frontend, ensuring the UI remains clean and environment-agnostic.

## 3. PTY Process Reliability

### The Problem
Complex "reader cloning" and attempts to "hand off" PTY handles led to process instability and deadlocks.

### The Solution: Durable Process Ownership
- **Spawn once, stay alive:** The backend now uses a simple "Spawn and Track" model. The reader thread stays alive for the duration of the process.
- **CLI-Native Resumption:** We leverage the agent's own `--resume <id>` capability for cross-restart persistence rather than trying to preserve PTY state in the app.
- **ID Capture:** The reader thread uses Regex to sniff for `session-id` patterns in the agent's output, automatically updating the database so the "Resume" button is always ready.

## 4. Database Concurrency

### The Problem
Deadlocks occurred when nested database calls (e.g., `create_project` calling `get_project_by_id`) both tried to lock the global DB Mutex.

### The Solution: Inner/Outer Helpers
We refactored the `db` module to use `_inner` functions that accept an existing `&Connection`, allowing "Outer" public functions to lock the mutex once and pass the connection through to any number of helper functions safely.
