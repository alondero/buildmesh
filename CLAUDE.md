Buildmesh is a Tauri 2 desktop app (React 19, Rust) for orchestrating AI coding agents (Anthropic, Minimax, Kimi, OpenCode, Antigravity, Codex) across repositories, with persistent xterm.js terminals and hybrid Windows/WSL support.

**`docs/knowledge-primer.md` is the source of truth for architecture, conventions, and anti-patterns. Read it before touching backend, terminal, agent-spawn, or path code.** Project domain language lives in `CONTEXT.md`; rationale in `docs/adr/*.md`. This file holds only the always-on rules.

## Commands
- Test: `npm test` (unit + integration) · `npm run test:e2e` (needs app on :1991) · `npm run test:ci` (all three)
- Typecheck/build: `npm run build` (runs `tsc`, desktop `vite build`, then mobile `vite build --mode mobile`)
- Rust: `cargo test` / `cargo clippy` (run inside `src-tauri/`)

## Hard rules — cause real breakage, do not violate
- **Never** call `.dispose()` on an xterm.js terminal unless the agent node is deleted → permanent terminal blanking. `TerminalManager` is a singleton; instances survive React remounts.
- **Never** pass Linux/WSL paths to Windows-side APIs. Convert via `env::to_host_path` (`src-tauri/src/env/mod.rs`); build `\\wsl$\` paths only inside that module.
- **Never** lock the DB mutex in nested calls → deadlock. Use `_inner(&Connection)` helpers; public fns lock once and pass the connection through (`src-tauri/src/db/mod.rs`).
- Don't replicate PTY-side `session-id` capture or node auto-naming — backend-only (`session_naming.rs`).
- New `#[command]` Tauri commands must be added to the `lib.rs` handler list, or they fail with "command not found" at runtime.

### Agent spawn shell wrappers (Windows)
Each provider adapter declares its `SpawnRecipe`; `spawn_environment::wrap` consumes it. Don't hard-code the shell — the adapter owns it.
- **cwrap providers** (Anthropic, Minimax, Kimi) → `powershell.exe` so ANSI escapes propagate
- **`.cmd` batch providers** (Antigravity, OpenCode) → `cmd.exe /c`
- **Codex** → `powershell.exe` on Windows, direct spawn on macOS/Linux
- On macOS/Linux, every provider uses `WindowsShell::Direct`; the Windows shell only matters on Windows.

A PreToolUse hook (`.claude/hooks/guard-antipatterns.mjs`) blocks edits that violate the dispose and WSL-path rules. Add `// allow-dispose` / `// allow-wsl-path` on the line only when the exception is genuinely correct.

## Code quality
- Match existing patterns. No new abstractions, deps, or speculative generality beyond the task.
- Add or update tests for behaviour changes (`tests/unit`, `tests/integration`).
- Comment only non-obvious *why*; let names carry the *what*.

## Pointers
- Architecture & anti-patterns (detailed): `docs/knowledge-primer.md`
- Domain language and mental model: `CONTEXT.md`
- DB schema: source of truth is `src-tauri/src/db/mod.rs` (`SCHEMA_VERSION`); tables `meshes`, `agent_nodes`, `checkpoints`.
- Verification: `/verify` — see `.claude/skills/verify/skill.md`
- Issues (`alondero/buildmesh`): `docs/agents/issue-tracker.md`; triage labels: `docs/agents/triage-labels.md`
