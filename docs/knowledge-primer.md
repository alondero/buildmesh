# Buildmesh — AI context index

> **Reading this file:** it is an index, not a manual. Find the row for the area
> you are changing, read that owner doc, then confirm the claim against the owning
> module (the code wins on any disagreement). Do not read the owner docs whole
> either — list their sections with `rg -n "^#{2,3} " <file>` first.
>
> `docs/agents/engineering.md` (how to work here) and `CONTEXT.md` (domain
> vocabulary) are the other two always-loaded documents.

## Tech Stack
- **Frontend:** React 19, Zustand 5, xterm.js 6.x, Tailwind 4, TypeScript ~5.8, Vite 7
- **Backend:** Tauri 2, Rust, portable-pty, rusqlite 0.32, git2, tokio
- **Testing:** Vitest (unit/integration) + Playwright (e2e)

## Project Structure
- `src/` — React frontend (Zustand stores, xterm.js TerminalManager)
- `src-tauri/src/` — Rust backend (commands/, db/, env/, git/, models/). All direct `git2` access lives in `git/` — `primitives` (dirty/ahead-behind/short-sha/head-branch), `worktree` (Worktree Node create/inspect/remove), `sync` (auto-sync), `health` (mesh drift/hostage/recovery); `commands/git.rs` & `prune.rs` are thin `#[command]` adapters over it (ADR 0007). `env/` owns detection and path conversion: `environment.rs` (Windows vs WSL detection, agent-CLI home dirs), `host_path.rs` (the only module allowed to build `\\wsl$\` paths, plus the `ResolvedPath` machinery), `mesh_row.rs` (mesh DTO read), and `windows_interop.rs` (Windows probes and callback commands from Linux WSL hosts).
- `tests/unit/` — Vitest unit tests
- `tests/integration/` — Vitest integration tests
- `tests/e2e/` — Playwright: webServer boots Vite on 1420; `verify-smoke` uses mock IPC, while chromium specs have additional real-runtime requirements. See `docs/agents/engineering.md` before launching.
- `scripts/ui-shot.mjs` — ad-hoc UI verification + screenshots: Playwright attaches over CDP to the real dev-profile window (`scripts\run-dev.ps1 -CdpPort 9223`); see `.claude/skills/verify-ui/SKILL.md`
- `docs/adr/` — Architecture Decision Records
- `docs/learning/` — Enduring technical deep dives, harness capability reviews, and integration contracts (e.g. `harness-capabilities-matrix.md` — the consolidated harness × capability grid, `agy-harness-capabilities.md`, `grok-harness-capabilities.md`, `mcode-harness-capabilities.md`, `opencode-harness-capabilities.md`, `muse-harness-capabilities.md`)

## Where the architecture lives

Each owner doc below was split out of this primer (issue #2045) and is now the
place to add or change that area's durable architecture.

For "which file do I open, and which boundary do I not cross", see the
[Rust module map](development/module-map.md) — a short map of the seams a new
reader should trust (commands are thin, git access lives in `git/`, the stepper
is pure, the worker is not).

| Area | Owner doc | Owning code |
|---|---|---|
| Model providers, usage meters, credentials, spawn recipes | [providers.md](development/providers.md) | `src-tauri/src/preferences/`, `src-tauri/src/services/usage.rs`, `src-tauri/src/services/windows_cred.rs`, `src-tauri/src/agent/provider/` |
| Terminals, xterm lifecycle, PTY input and output streaming | [terminals.md](development/terminals.md) | `src/components/Terminal/`, `src-tauri/src/pty/` |
| Agent Node lifecycle, process registry, worktrees | [agent-nodes.md](development/agent-nodes.md) | `src-tauri/src/services/agent_node.rs`, `src-tauri/src/agent/`, `src-tauri/src/git/worktree/` |
| Attention system, hooks, turn counting, node naming | [attention.md](development/attention.md) | `src-tauri/src/http/routes/attention.rs`, `src-tauri/src/commands/attention.rs`, `src-tauri/src/agent/session_lifecycle/` |
| Probe panel, view modes, context lenses | [probe-ui.md](development/probe-ui.md) | `src/components/Probe/`, `src/stores/` |
| Windows, WSL paths, frameless window, shortcuts | [windows.md](development/windows.md) | `src-tauri/src/env/host_path.rs`, `src/components/TitleBar/` |
| Startup, profiles, crash recovery, environment detection | [startup-and-profiles.md](development/startup-and-profiles.md) | `src-tauri/src/lib.rs`, `src-tauri/src/instance_guard.rs`, `src-tauri/src/env/environment.rs` |
| Autopilot circuits | [circuits.md](development/circuits.md) | `src-tauri/src/circuit/`, `src-tauri/src/services/circuit_worker/`, `src-tauri/src/db/circuit/` |
| Coordinator API, LAN/VPN exposure | [remote-access.md](development/remote-access.md) | `src-tauri/src/http/`, `src-tauri/src/coordinator/` |
| State recovery: snapshot, export, integrity, restore | [state-recovery.md](development/state-recovery.md) | `src-tauri/src/services/state_recovery/` |
| Rust conventions: DB, threading, caches, guards, wire types | [rust-conventions.md](development/rust-conventions.md) | `src-tauri/src/db/`, `src-tauri/src/commands/`, `src/types/generated/` |
| Mobile client | [mobile.md](development/mobile.md) | `src/mobile/`, `src-tauri/src/http/routes/` |

Supporting detail that is a current contract rather than architecture: the
[development guide](development/README.md), [releasing](development/releasing.md),
[supply-chain controls](development/supply-chain.md),
[circuit effect recovery](development/circuit-effect-recovery.md),
[circuits from Agent Nodes](development/agent-node-circuits.md),
[node status observation](development/node-status-observation.md),
[coordinator read API](development/coordinator-read-api.md),
[Android client](development/android.md), and the
[Probe UI checklist](development/probe-ui-checklist.md).

Dated investigations and run write-ups are historical records, not current
contracts: they live under [archive/](archive/README.md).

## Anti-Patterns (DO NOT do)
- ❌ Call `dispose()` on an xterm.js Terminal — causes permanent terminal blanking
- ❌ Pass Linux paths (e.g. `/home/user/`) to non-WSL APIs — causes "file not found"
- ❌ Spawn a provider CLI to fetch a Usage Meter when the CLI is wrapping an HTTP endpoint we can call ourselves. `get_provider_meters` waits for every provider, so a multi-second CLI boot stalls the whole Usage Probe (#1324 spawned `agy --print /usage` ≈6s; the same payload is `POST daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary` in ~250ms, User-Agent gated). Token discovery lives in `usage::adapters::agy`: prefer `<agy_dir>/antigravity-oauth-token` (Windows: `%USERPROFILE%\.gemini\antigravity-cli\antigravity-oauth-token`, honouring `GEMINI_HOME` / `ANTIGRAVITY_HOME`), then Windows Credential Manager `gemini:antigravity`. Corrupt CLI-file JSON surfaces as Shape (no keyring fallback); missing/empty falls through. On HTTP 401/403, try the next source before logged-out. `fetchAvailableModels` is five-hour-only fallback.
- ❌ Lock the DB mutex in nested calls — causes deadlocks
- ❌ Do blocking network / git-shell-out / slow-libgit2 / SQLite (`db::*`) / `std::fs::*` / `preferences::load`/`save` work directly on an `async fn` (or `#[command(async)]`) command — it parks a tokio worker and, at scale, starves the pool (UI stays alive, keystrokes + WebSocket streaming + probes hang). Use the `*_blocking` sync-core + `run_blocking` wrapper; see [Command Threading](development/rust-conventions.md#command-threading-blocking-work-must-not-touch-the-async-worker-pool) (issue #1380).
- ❌ Give a Probe tab root its own `overflow-y-auto` — `ProbePanel` already wraps it in one, so you get two stacked scroll owners and an unpredictable scroll surface (#1468). Root is layout-only; one inner body scrolls. And don't `truncate` unbounded text (errors, trigger identities, node ids) at the dock's 240px minimum — it clips exactly the tail that carries the diagnosis. See [Probe Panel shell](development/probe-ui.md#probe-panel-shell-scroll-ownership-narrow-width).
- ❌ Compare a zoneless SQLite timestamp (`"2026-08-22 10:05:00"`, what `CURRENT_TIMESTAMP` writes) against `Date.now()` via a bare `Date.parse`. V8 accepts the shape and reads it as **local** time, so the value is silently wrong by the host's UTC offset. The skew cancels when you subtract two ledger timestamps — which is why `stepDurationMs` hid it for months — but not against an absolute clock. Parse through `ledgerTimestampMs` in `circuitGraphModel.ts`, which forces `Z` on any zoneless timestamp.
- ❌ Ship `<a target="_blank">` for an external URL — Tauri 2's WebView is not a browser, the click is silently dropped without the `core:webview:allow-create-webview-window` capability (which we don't grant). Keep the `href`/`target`/`rel` and route the `onClick` through `openUrl()` from `@tauri-apps/plugin-opener` (e.g. `src/components/SessionView/GridNodeHeader.tsx:145`). The right-click "Open in browser" path still works, which makes the bug look like a click-handler issue — it isn't.
- ❌ Read a request body with bare `BufStream::read_exact` (or `read_line` for the head) without a `tokio::time::timeout` wrapper. A client that advertises a Content-Length and dribbles bytes pins a tokio worker for the entire upload window — a slowloris that hits every POST body and every WebSocket header read. The single seam is `crate::http::request::read_body_with_cap` (invoked by `http::server::handle_connection` from the route's `BodyPolicy`); `REQUEST_HEAD_TIMEOUT` wraps the head read in that same function. Route handlers take `ParsedRequest` (body already read) and must not reimplement the read.
- ❌ Store a Web API as `this.x = requestAnimationFrame` (or `setTimeout` / `fetch` / `MutationObserver` / etc.). Chromium WebIDL bindings enforce the receiver — calling the API through an object property throws `TypeError: Illegal invocation` and the throw lands inside a Tauri listener that swallows it, so the symptom looks like "events not arriving" rather than a crash. Always wrap: `this.scheduler = (cb) => requestAnimationFrame(cb)` (the form `TerminalWriter` uses, in `src/components/Terminal/TerminalWriter.ts`). Pinned by `tests/unit/webapi-on-this.test.ts`; opt-out `// allow-webapi-on-this: <reason>` on the violation line. Memory: `buildmesh-webapi-receiver-binding`.
- ❌ Nest a `position:fixed` overlay (context menu, dialog) inside an ancestor that has `filter` (`hover:brightness-*`), `transform` (dnd-kit sortable), `opacity` other than 1, or `backdrop-filter`. Those properties create a containing block, so `top`/`left` from `clientX`/`clientY` are no longer viewport coordinates — the overlay jumps, then auto-focus scrolls the nearest `overflow` ancestor. Portal to `document.body`. The shared `Modal` primitive already does this (issue #1292), so `<Modal>` and `<ConfirmDialog>` (its thin wrapper) are safe to mount anywhere; only click-anchored menus (`NodeItem` / `MeshItem` sidebar context menus) still need to portal at the call site. Don't put `preventScroll` on the shared `useAriaMenu` hook: `ProviderDropdown` is itself `overflow-y-auto` and needs default focus-scroll so arrow keys can reach items below the fold.
