# Buildmesh — AI context index

> **Reading this file:** it is an index, not a manual. Find the row for the area
> you are changing, read that owner doc, then confirm the claim against the owning
> module (the code wins on disagreement). List owner doc sections first with
> `rg -n "^#{2,3} " <file>`; do not read them whole.
>
> `docs/agents/engineering.md` (how to work here) and `CONTEXT.md` (domain
> vocabulary) are the other two always-loaded documents.

## Tech Stack
- **Frontend:** React 19, Zustand 5, xterm.js 6.x, Tailwind 4, TypeScript ~5.9, Vite 8
- **Backend:** Tauri 2, Rust, portable-pty, rusqlite 0.41, git2, tokio
- **Testing:** Vitest (unit/integration) + Playwright (e2e)

## Project Structure
- `src/` — React frontend (Zustand stores, xterm.js TerminalManager)
- `src-tauri/src/` — Rust backend (commands/, db/, env/, git/, models/). Module ownership and seams live in the [Rust module map](development/module-map.md). `env/host_path.rs` is the only module allowed to build `\\wsl$\` paths.
- `tests/unit/` — Vitest unit tests
- `tests/integration/` — Vitest integration tests
- `tests/e2e/` — Playwright: webServer boots Vite on 1420; `verify-smoke` uses mock IPC, while chromium specs have additional real-runtime requirements. See `docs/agents/engineering.md` before launching.
- `scripts/ui-shot.mjs` — ad-hoc UI verification + screenshots: Playwright attaches over CDP to the real dev-profile window (`scripts\run-dev.ps1 -CdpPort 9223`); see `.claude/skills/verify-ui/SKILL.md`
- `docs/adr/` — Architecture Decision Records
- `docs/learning/` — Enduring technical deep dives, harness capability reviews, and integration contracts (e.g. `harness-capabilities-matrix.md` — the consolidated harness × capability grid, `muse-harness-capabilities.md`)

## Where the architecture lives

Each owner doc below was split out of this primer (issue #2045) and is now the
place to add or change that area's durable architecture.

For backend responsibility ownership, see the
[Rust module map](development/module-map.md#responsibility-modules).

| Area | Owner doc | Owning code |
|---|---|---|
| Frontend IPC facets, transport ownership, live event streams | [frontend-ipc.md](development/frontend-ipc.md) | `src/lib/tauri/`, `src/lib/tauri.ts`, `src/lib/streams/`, `eslint.config.js` |
| Model providers, usage meters, credentials, spawn recipes | [providers.md](development/providers.md) | `src-tauri/src/preferences/`, `src-tauri/src/services/usage/adapters/`, `src-tauri/src/services/windows_cred.rs`, `src-tauri/src/agent/provider/` |
| Terminals, xterm lifecycle, PTY input and output streaming | [terminals.md](development/terminals.md) | `src/components/Terminal/`, `src-tauri/src/pty/` |
| Agent Node lifecycle, process registry, worktrees | [agent-nodes.md](development/agent-nodes.md) | `src-tauri/src/services/agent_node.rs`, `src-tauri/src/agent/`, `src-tauri/src/git/worktree/` |
| Attention system, hooks, turn counting, node naming | [attention.md](development/attention.md) | `src-tauri/src/http/routes/attention.rs`, `src-tauri/src/commands/attention.rs`, `src-tauri/src/agent/session_lifecycle/` |
| Probe panel, view modes, context lenses | [probe-ui.md](development/probe-ui.md) | `src/components/Probe/`, `src/stores/` |
| Git query cache: freshness and retention | [git-query-cache.md](development/git-query-cache.md) | `src/lib/pathInvalidatedCache.ts`, `src/hooks/usePathInvalidatedQuery.ts`, `src-tauri/src/git/status_stats.rs` |
| Windows, WSL paths, frameless window, shortcuts | [windows.md](development/windows.md) | `src-tauri/src/env/host_path.rs`, `src/components/TitleBar/`, `src/lib/windowFocus.ts` |
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

Git-backed project folders use `git::project` for nearest-repository discovery
and common-directory identity. `env::node_working_path` reconstructs saved scope
for launch; `node_git_path` retains query/watcher identity and
`node_worktree_path` retains checkout ownership. See
[Agent Node worktree support](development/agent-nodes.md#worktree-support-git2-based).

## Anti-Patterns (DO NOT do)
- Failed automatic launches stay `error`; restoring `idle` after spawn rejection re-arms the terminal launch effect. Explicit retry owns a new request.

- ❌ Call `dispose()` on an xterm.js Terminal — causes permanent terminal blanking
- ❌ Pass Linux paths (e.g. `/home/user/`) to non-WSL APIs — causes "file not found"
- ❌ Spawn a provider CLI to fetch a Usage Meter when the CLI is wrapping an HTTP endpoint we can call ourselves. `get_provider_meters` waits for every provider, so a multi-second CLI boot stalls the whole Usage Probe (#1324 spawned `agy --print /usage` ≈6s; the same payload is `POST daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary` in ~250ms, User-Agent gated). Token discovery lives in `usage::adapters::agy`: prefer `<agy_dir>/antigravity-oauth-token` (Windows: `%USERPROFILE%\.gemini\antigravity-cli\antigravity-oauth-token`, honouring `GEMINI_HOME` / `ANTIGRAVITY_HOME`), then Windows Credential Manager `gemini:antigravity`. Corrupt CLI-file JSON surfaces as Shape (no keyring fallback); missing/empty falls through. On HTTP 401/403, try the next source before logged-out. `fetchAvailableModels` is five-hour-only fallback.
- ❌ Lock the DB mutex in nested calls — causes deadlocks
- ❌ Treat a reader-pool checkout as infallible — `db::read_conn()` returns a typed `SQLITE_BUSY` error when the eight-reader pool is momentarily empty, and `panic = "abort"` turns an `expect`ed checkout into a whole-app crash (issue #1533). Propagate it: HTTP → `503`, command → the retryable busy copy. See [rust-conventions](development/rust-conventions.md#a-reader-pool-exhaustion-is-an-error-never-a-panic-issue-1533).
- ❌ Shell out to procps `kill` with a negative process-group id. Ubuntu 24.04's `kill -KILL -<pid>` (no `--`) keeps only the first digit, so a pid starting with 1 becomes `-1` and signals every process the user can kill — including the CI runner, which is why the `services` shard lost its runner with no log (issue #2103). Use `process_util::kill_process_group` (`kill(2)` with the whole id). See [Process-group signals](development/rust-conventions.md#process-group-signals).
- ❌ Do blocking network / git-shell-out / slow-libgit2 / SQLite (`db::*`) / `std::fs::*` / `preferences::load`/`save` work directly on an `async fn` (or `#[command(async)]`) command — it parks a tokio worker and, at scale, starves the pool (UI stays alive, keystrokes + WebSocket streaming + probes hang). Use the `*_blocking` sync-core + `run_blocking` wrapper; see [Command Threading](development/rust-conventions.md#command-threading-blocking-work-must-not-touch-the-async-worker-pool) (issue #1380).
- ❌ Give a Probe tab root its own `overflow-y-auto` — `ProbePanel` already wraps it in one, so you get two stacked scroll owners and an unpredictable scroll surface (#1468). Root is layout-only; one inner body scrolls. And don't `truncate` unbounded text (errors, trigger identities, node ids) at the dock's 240px minimum — it clips exactly the tail that carries the diagnosis. See [Probe Panel shell](development/probe-ui.md#probe-panel-shell-scroll-ownership-narrow-width).
- ❌ Compare a zoneless SQLite timestamp (`"2026-08-22 10:05:00"`, what `CURRENT_TIMESTAMP` writes) against `Date.now()` via a bare `Date.parse`. V8 accepts the shape and reads it as **local** time, so the value is silently wrong by the host's UTC offset. The skew cancels when you subtract two ledger timestamps — which is why `stepDurationMs` hid it for months — but not against an absolute clock. Parse through `ledgerTimestampMs` in `circuitGraphModel.ts`, which forces `Z` on any zoneless timestamp.
- ❌ Ship `<a target="_blank">` for an external URL — Tauri 2's WebView is not a browser, the click is silently dropped without the `core:webview:allow-create-webview-window` capability (which we don't grant). Keep the `href`/`target`/`rel` and route the `onClick` through `openUrl()` from `@tauri-apps/plugin-opener` (e.g. `src/components/SessionView/GridNodeHeader.tsx:145`). The right-click "Open in browser" path still works, which makes the bug look like a click-handler issue — it isn't.
- ❌ Read a request body with bare `BufStream::read_exact` (or `read_line` for the head) without a `tokio::time::timeout` wrapper. A client that advertises a Content-Length and dribbles bytes pins a tokio worker for the entire upload window — a slowloris that hits every POST body and every WebSocket header read. The single seam is `crate::http::request::read_body_with_cap` (invoked by `http::server::handle_connection` from the route's `BodyPolicy`); `REQUEST_HEAD_TIMEOUT` wraps the head read in that same function. Route handlers take `ParsedRequest` (body already read) and must not reimplement the read.
- ❌ Return raw file text as HTML from a highlighting/render fallback. The diff view injects server-rendered strings with `dangerouslySetInnerHTML` (`src/components/Diff/Diff.tsx`) and the CSP is `null` (#830), so a fallback that returns unescaped contents turns markup in any opened file into live script. Every arm that yields HTML must escape the text itself — syntect escapes only its success path, which is why the per-line fallback escapes and the whole-string fallback must too (#2152). Trust a highlighter's success output, never its error output.
- ❌ Store a Web API as `this.x = requestAnimationFrame` (or `setTimeout` / `fetch` / `MutationObserver` / etc.). Chromium WebIDL bindings enforce the receiver — calling the API through an object property throws `TypeError: Illegal invocation` and the throw lands inside a Tauri listener that swallows it, so the symptom looks like "events not arriving" rather than a crash. Always wrap: `this.scheduler = (cb) => requestAnimationFrame(cb)` (the form `TerminalWriter` uses, in `src/components/Terminal/TerminalWriter.ts`). Pinned by `tests/unit/webapi-on-this.test.ts`; opt-out `// allow-webapi-on-this: <reason>` on the violation line. Memory: `buildmesh-webapi-receiver-binding`.
- ❌ Nest a `position:fixed` overlay (context menu, dialog) inside an ancestor that has `filter` (`hover:brightness-*`), `transform` (dnd-kit sortable), `opacity` other than 1, or `backdrop-filter`. Those properties create a containing block, so `top`/`left` from `clientX`/`clientY` are no longer viewport coordinates — the overlay jumps, then auto-focus scrolls the nearest `overflow` ancestor. Portal to `document.body`. The shared `Modal` primitive already does this (issue #1292), so `<Modal>` and `<ConfirmDialog>` (its thin wrapper) are safe to mount anywhere; only click-anchored menus (`NodeItem` / `MeshItem` sidebar context menus) still need to portal at the call site. Don't put `preventScroll` on the shared `useAriaMenu` hook: `ProviderDropdown` is itself `overflow-y-auto` and needs default focus-scroll so arrow keys can reach items below the fold.
- ❌ Treat a non-empty `agent_nodes.cli_session_id` as proof that a conversation exists, or re-derive "should I resume?" anywhere but the one decision point. `prepare_context` pre-writes that UUID *before* the harness runs, so the column advertises a resume target that may never have a transcript — and the frontend's intent resolver turns any non-empty value into `--resume <id>`, which exits immediately against a missing conversation and is relaunched on the spot (issue #2137: ~160 relaunches in ~4 s intervals). A `--resume` that dies inside `resume_guard::UNUSABLE_RESUME_WINDOW` is the evidence that invalidates the identity; `agent::spawn::resume_guard` owns that, and the reader epilogue is the only place that can apply it because it is the only place that sees a process death next to the launch mode. Equally: don't "fix" such a loop by fencing `kill_agent_blocking`'s `on_idle` behind `FORBIDDEN_TERMINAL` — that unfenced `Idle` write is what clears a recovery `Error` so an explicit Retry Resume / Start Fresh can spawn at all. See [Agent Node lifecycle](development/agent-nodes.md).
- ❌ Treat a client poll as a per-read derivation. The Android client re-reads its snapshot on a 5s timer while foregrounded, so a paired phone idling on the node list re-derived the whole Spawn Menu — per-runtime Codex probe included, and a failing probe is discarded rather than served warm — twelve times a minute, each pass writing a log line over the attention and lifecycle lines it was meant to help read (issue #2349). Memoize the derivation behind its read path (`agent::provider_menu::derived_menu`, keyed on the preferences generation and the detection epoch), and don't expect a quieter level to fix a rate: the default filter already admits this crate at `debug`, so only a cache removes the lines. Same shape applies to any route a remote client re-reads on a timer. See [providers.md](development/providers.md#the-spawn-menu-derivation-reads-one-preferences-snapshot-and-one-alias-table).
- ❌ Replace a whole collection from a snapshot response without proving the response is still the newest thing the store knows. `meshStore` applied every `list_meshes` result unconditionally, so a read that started before a rename, create, delete or drag resolved afterwards and undid the acknowledged write — saved Mesh settings flickering back, deleted rows returning, a new row vanishing (issue #2326). Request identity, a revision every committed mutation advances, and pending-optimistic authority belong in the store, not in the caller that happens to overlap a read. Drop an obsolete response and converge it with one trailing read, or you have traded a stale write for stale state. See [Mesh snapshot authority](development/probe-ui.md#mesh-snapshot-authority-issue-2326).
- ❌ Treat a provider session id that differs from `agent_nodes.cli_session_id` as proof of a retired process, or as proof of a continuation. It is the same shape in both directions: Claude Code can carry a *running* process into a new native session and record the transition as `{"type":"continued-in",…}` in the predecessor's transcript, so every later callback carries an id Buildmesh never stored — drop all of them as stale and the run looks permanently busy while its Circuit can never observe its own report (issue #2263). Reconcile from that record through `http/routes/attention/continuation.rs`, never from a payload field, and never from an id alone: an unrelated id, a reversed record, or an id the chain has already passed is a retired process. See [Attention system](development/attention.md#native-session-continuation-issue-2263).
