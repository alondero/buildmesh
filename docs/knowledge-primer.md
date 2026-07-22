# Buildmesh — AI Context

## Tech Stack
- **Frontend:** React 19, Zustand 5, xterm.js 6.x, Tailwind 4, TypeScript ~5.8, Vite 7
- **Backend:** Tauri 2, Rust, portable-pty, rusqlite 0.32, git2, tokio
- **Testing:** Vitest (unit/integration) + Playwright (e2e)

## Project Structure
- `src/` — React frontend (Zustand stores, xterm.js TerminalManager)
- `src-tauri/src/` — Rust backend (commands/, db/, env/, git/, models/). All direct `git2` access lives in `git/` — `primitives` (dirty/ahead-behind/short-sha/head-branch), `worktree` (Worktree Node create/inspect/remove), `sync` (auto-sync), `health` (mesh drift/hostage/recovery); `commands/git.rs` & `prune.rs` are thin `#[command]` adapters over it (ADR 0007). `env/` is detection + path conversion split into three sub-modules (issue #248): `environment.rs` (Windows vs WSL detection, agent-CLI home dirs), `host_path.rs` (the only module allowed to build `\\wsl$\` paths, plus the `ResolvedPath` machinery), `mesh_row.rs` (mesh DTO read).
- `tests/unit/` — Vitest unit tests
- `tests/integration/` — Vitest integration tests
- `tests/e2e/` — Playwright e2e (requires app running on port 1991; boots `tauri dev` on the **base** identity — collides with a running stable hub, so not for autonomous agents)
- `scripts/ui-shot.mjs` — ad-hoc UI verification + screenshots: Playwright attaches over CDP to the real dev-profile window (`scripts\run-dev.ps1 -CdpPort 9223`); see `.claude/skills/verify-ui/skill.md`
- `docs/adr/` — Architecture Decision Records

## Key Conventions

### Terminal Persistence (CRITICAL)
`TerminalManager` is a **singleton**. xterm.js instances survive React remounts via a hidden container stack. Never call `dispose()` on a terminal unless the agent node is explicitly deleted — see `src/components/Terminal/Terminal.tsx`. Disposing a terminal causes permanent blanking.

### Layout: Grid-Only
Single layout was removed 2026-04-29. Only `grid` layout (split-panes) is valid. The UI auto-scales 1–6 panes via CSS grid.

### Frameless Window & Bespoke TitleBar
The window runs with `"decorations": false` (`src-tauri/tauri.conf.json`); `src/components/TitleBar/TitleBar.tsx` is the window chrome (wordmark, ViewModeSwitcher, settings/remote icons, min/max/close). Three traps this recipe already burned once:
- **Drag regions are per-target.** Tauri's injected script checks `e.target.hasAttribute('data-tauri-drag-region')` — put the attribute on the bar/spacer/wordmark, but *never* on buttons or their SVGs, or the click is eaten and the button starts a drag instead. Double-click maximize on a drag region is built into the same script (`internal_toggle_maximize`).
- **`core:window:default` does NOT cover `allow-minimize`, `allow-close`, `allow-toggle-maximize`, or `allow-start-dragging`** — a frameless window's controls silently no-op without them. Add them explicitly in `src-tauri/capabilities/default.json` (done; keep them if the capability file is regenerated).
- **One writer for window state.** Track `isMaximized` only via the `onResized` listener re-querying `win.isMaximized()`; don't optimistically flip local state on click — a rejected IPC desyncs the glyph.

### WSL Path Mapping
Linux paths from WSL agents must map to Windows UNC paths (`\\wsl$\...`) before backend file operations. Use `env::to_host_path` in `src-tauri/src/env/host_path.rs` (the `HostPath` sub-module). The CLAUDE.md hard rule is **structurally** enforced: `HostPath` is the *only* module in the tree that builds `\\wsl$\` or `/mnt/` strings; no other module should. Never pass Linux paths to Windows-side APIs.

### Agent Spawning on Windows
Anthropic and Minimax use `cwrap` spawned via `cmd.exe /c` — **not** direct. Antigravity and OpenCode are spawned **directly** (no cwrap). See `src-tauri/src/commands/agent.rs`.

### Database Pattern
Use `_inner` helper functions that accept `&Connection` to avoid mutex deadlocks. Public functions lock once and pass the connection through. See `src-tauri/src/db/mod.rs`.

### Command Threading (blocking work must not touch the async worker pool)
A `#[command]` on an `async fn` **and** `#[command(async)]` on a sync `fn` both run on Tauri's bounded tokio worker pool (≈ CPU cores). Only a plain sync `#[command] fn` runs off it. So a command that does a **blocking network call** (`reqwest::blocking`, `git fetch`/`git pull` shell-out) or slow libgit2 walk on the async runtime **parks a worker for the whole duration**; enough of them stuck at once starves the pool and every other async command (agent keystrokes, probes) stops being polled while the UI stays alive — the class of bug behind the overnight-freeze (issue: see the reqwest-timeout + spawn-blocking fixes in `commands/pr.rs`, `commands/github.rs`, `services/github.rs`). Convention: give each such command a **plain-sync core (`*_blocking`)** and a thin `#[command] async fn` wrapper that offloads it via `crate::commands::run_blocking(label, || core(..))` (which threads it through `tauri::async_runtime::spawn_blocking`). The mobile HTTP routes (`http/routes/*`) are **not** a separate pool — `http/mod.rs` spawns each connection on the same `tauri::async_runtime`, so a route that calls a `*_blocking` core directly still parks a worker; routes are `async fn`, so they must **`.await` the async command wrapper** (e.g. `get_repo_issues(id).await`), letting it offload. Only reach for a `*_blocking` core from a genuinely synchronous context (e.g. `check_gh_auth_cached`, itself run inside `run_blocking`). Also give any blocking network client a finite `.timeout(..)` (`GitHubClient` uses `build_http_client`) so a half-open connection can't hang forever.

### Pattern Guards (lint-style unit tests)
The repo runs a small fleet of "pattern guard" unit tests that walk source files and fail on text patterns known to ship a silent regression. They live under `tests/unit/` and run in the standard `npm test` / `scripts\check.ps1 unit` loop:

- `tests/unit/ipc-contract.test.ts` (issue #163) — every `invoke('name', …)` literal in `src/` must be registered in `tauri::generate_handler![…]` in `src-tauri/src/lib.rs`. Companion: each `#[command]` MUST be added to that handler list too (the symmetric trap).
- `tests/unit/webapi-on-this.test.ts` (issue #156) — forbids `this.x = <WebAPI>` (e.g. `this.scheduler = requestAnimationFrame`). In Chromium/WebView2 the WebIDL receiver binding throws "Illegal invocation" when the API is invoked through an object property. Fix: wrap the API in an arrow function (`this.scheduler = (cb) => requestAnimationFrame(cb)`) or `.bind(window)`. Per-line opt-out: `// allow-webapi-on-this: <reason>`, mirroring the `// allow-dispose` / `// allow-wsl-path` convention enforced by `.claude/hooks/guard-antipatterns.mjs`. See memory `buildmesh-webapi-receiver-binding` for the full receiver-binding story.
- `tests/unit/guard-antipatterns.test.ts` — unit-tests the pure helpers exported by `.claude/hooks/guard-antipatterns.mjs` (`checkContentViolations`, `checkWorktreeEscape`).

CI additionally runs a Rust-side text-pattern guard in `.github/workflows/build.yml` (no `std::process::Command::new("git")` and no inline `.creation_flags(` outside `process_util.rs`, per issue #665 / #690). The opt-out comment `// allow-inline-process-spawn: <reason>` is honored only by that CI step.

When writing a new pattern guard, mirror `ipc-contract.test.ts`: walk the tree, strip comments, apply a regex, report `file:line` violations in the failure message, and synthetic-test each input case so the regex is provably not a placebo. Test the *opt-out* path (escape hatch honored) and the *negative* path (hatch on a wrong line is NOT honored) — same pattern as the `// allow-dispose` tests in `guard-antipatterns.test.ts`.

### Shared Rust↔TS Types (wire-shape source of truth)
Wire types that cross the Tauri `invoke` boundary **or** the mobile HTTP server are generated from Rust with [`ts-rs`](https://github.com/Aleph-Alpha/ts-rs), not hand-declared in TS. The Rust struct is the single source of truth (issue #359).

- **Producing a type:** add `TS` to the derive list and `#[ts(export, export_to = "Name.ts")]` to the struct/enum (e.g. `models::Mesh`, `models::AgentNode`, the `EnvType`/`Provider`/`SessionStatus` enums, `commands::pr::GitHubIssue`, `commands::git::GitStatus`, `services::session_discovery::DiscoveredSession`).
- **Generation:** `cargo test` (run in `src-tauri/`) runs ts-rs's auto-generated `export_bindings_*` tests, which write `.ts` files to `src/types/generated/`. The dir is set by `TS_RS_EXPORT_DIR` in `src-tauri/.cargo/config.toml`. **Generated files are committed** and must never be hand-edited (they carry a "Do not edit" banner).
- **Consuming a type:** import from `src/types/generated/`. Stores and `src/lib/tauri.ts` / `src/mobile/api.ts` re-export the generated type under the name call sites already use.
- **`i64`/`u64`/`usize` → `#[ts(as = "i32")]`** (and `Option<i64>` → `#[ts(as = "Option<i32>")]`). ts-rs defaults 64-bit ints to `bigint`, but serde_json sends them as JS numbers; the annotation makes the generated type say `number`. Forgetting it produces `bigint`, which fails the TS build — drift caught, not shipped.
- **`Vec<i64>` on the wire — use `Vec<i32>`.** There's no precedent in this codebase for a `Vec<i64>` field on a ts-rs-exported struct; ts-rs generates `Array<bigint>` from `Vec<i64>` directly, breaking the JSON-over-IPC contract. When a ts-rs-exported struct needs a list of integer IDs (e.g. issue #481's "blocked-by" list), use `Vec<i32>` on the wire struct — it matches the per-element `#[ts(as = "i32")]` cast convention and ts-rs emits `Array<number>` natively. Keep the internal `services::*` struct as `Vec<i64>` (GitHub's native integer width) and downcast in the command mapper with `.map(|n| n as i32).collect()`. `#[serde(default)]` on a `Vec<T>` field deserialises a missing key to `vec![]`, keeping the wire additive across rolling deploys — the wire changes without a coordinated frontend cutover.
- **serde attributes are honoured** (ts-rs `serde-compat`, on by default): `#[serde(rename_all = "snake_case")]` on `SessionStatus` makes the union `"awaiting_input"`, matching the DB and frontend. (A `rename_all = "lowercase"` here silently emitted `"awaitinginput"` — the exact drift class #359 closes.)
- **CI gate:** `.github/workflows/build.yml` runs `cargo test` then `git diff --exit-code src/types/generated`. A Rust struct change that isn't reflected in committed bindings fails the build.
- **Still hand-maintained (migrate later):** `src/lib/status.ts`'s `SessionStatus` (a UI-config copy), and the `Diff*`/`FileNode`/`OpenPr`/`GitBranchStatus` types in `tauri.ts`/`api.ts`. These are not yet generated.

## Anti-Patterns (DO NOT do)
- ❌ Call `dispose()` on an xterm.js Terminal — causes permanent terminal blanking
- ❌ Pass Linux paths (e.g. `/home/user/`) to non-WSL APIs — causes "file not found"
- ❌ Spawn cwrap directly without `cmd.exe /c` on Windows — ConPTY breaks
- ❌ Lock the DB mutex in nested calls — causes deadlocks
- ❌ Do blocking network / git-shell-out / slow-libgit2 work directly on an `async fn` (or `#[command(async)]`) command — it parks a tokio worker and, at scale, starves the pool (UI stays alive, keystrokes + probes hang). Use the `*_blocking` sync-core + `run_blocking` wrapper; see *Command Threading*
- ❌ Hand-declare a TS interface for a Rust wire type, or hand-edit a file in `src/types/generated/` — derive `TS` on the Rust struct and import the generated type instead (issue #359)
- ❌ Ship `<a target="_blank">` for an external URL — Tauri 2's WebView is not a browser, the click is silently dropped without the `core:webview:allow-create-webview-window` capability (which we don't grant). Keep the `href`/`target`/`rel` and route the `onClick` through `openUrl()` from `@tauri-apps/plugin-opener` (e.g. `src/components/SessionView/GridNodeHeader.tsx:145`). The right-click "Open in browser" path still works, which makes the bug look like a click-handler issue — it isn't.
- ❌ Read a request body with bare `BufStream::read_exact` (or `read_line` for the head) without a `tokio::time::timeout` wrapper. A client that advertises a Content-Length and dribbles bytes pins a tokio worker for the entire upload window — a slowloris that hits every POST body and every WebSocket header read. The single seam is `crate::http::request::read_body_or_send_error` for bodies and the `REQUEST_HEAD_TIMEOUT` wrap in `handle_connection` (http/mod.rs); every new route's body read must go through them, not reimplement the read.
- ❌ Store a Web API as `this.x = requestAnimationFrame` (or `setTimeout` / `fetch` / `MutationObserver` / etc.). Chromium WebIDL bindings enforce the receiver — calling the API through an object property throws `TypeError: Illegal invocation` and the throw lands inside a Tauri listener that swallows it, so the symptom looks like "events not arriving" rather than a crash. Always wrap: `this.scheduler = (cb) => requestAnimationFrame(cb)` (the form `TerminalWriter` uses, `src/components/Terminal/TerminalWriter.ts:60`). Pinned by `tests/unit/webapi-on-this.test.ts`; opt-out `// allow-webapi-on-this: <reason>` on the violation line. Memory: `buildmesh-webapi-receiver-binding`.

## Credentials (Windows Credential Manager)

The Buildmesh-managed OAuth secrets live in Windows Credential Manager under `CRED_TYPE_GENERIC` (the catch-all "store arbitrary bytes" type — domain credentials are a separate `CRED_TYPE_DOMAIN_*` family and we never use them). FFI is hand-rolled over `advapi32!CredReadW` / `CredWriteW` / `CredDeleteW` rather than `windows-sys`, matching the project's "minimal-FFI for the two-or-three functions we actually call" convention (also used by `sandbox::restricted_token`).

- **Surface** (`src-tauri/src/services/windows_cred.rs`):
  - `read(target: &str) -> Result<Vec<u8>, UsageError>` — missing credential collapses to `NoCredential(target)`; empty blob bytes round-trip as `Vec::new()` so a higher-level parser can decide what "empty" means.
  - `write(target: &str, blob: &[u8]) -> Result<(), UsageError>` — upsert via `CredWriteW` with `CRED_PERSIST_LOCAL_MACHINE` (persists across reboots, local-user-scoped; never `CRED_PERSIST_SESSION` — OAuth tokens need to survive logoff — and never `CRED_PERSIST_ENTERPRISE`, which requires domain policy we don't ship). `UserName` is set to the same string as the target so Credential Manager's detail view is manageable.
  - `delete(target: &str) -> Result<(), UsageError>` — **idempotent**: a `FALSE` return followed by `GetLastError() == ERROR_NOT_FOUND (1168)` collapses to `Ok(())` so the Settings "Sign out" affordance never errors on a no-op. Any other Windows failure surfaces as `Shape(target, GetLastError)`.
  - `cfg(windows)` only. Non-Windows callers see `NoCredential(...)` from their `cfg`-gated helpers instead.

- **Known targets** (extend-only — never delete from this list without a migration ticket):
  - `gemini:antigravity` — written by the Antigravity CLI, read-only here (issue #917).
  - `opencode:console` — written by Buildmesh for the OpenCode Go OAuth dance (issue #956). Persisted blob is JSON `{ access_token, workspace_id, refresh_token, expires_at, server_id }` (RFC-3339 string for `expires_at`, mirroring the original #957 fixture so the live probe still parses; the `server_id` field is the SolidStart deployment id captured into the `X-Server-Id` header).

- **Operator commands** for diagnosing drift:
  - `cmdkey /list:opencode:console` — read the current blob's user/credential metadata without dumping bytes.
  - `cmdkey /list:buildmesh-test-*` — find any leftover test credentials from a failing test that didn't clean up. Each unit test uses a uuid-suffixed target name so collisions are vanishingly rare.

- **Pitfalls** — each caught by an iteration of real bugs:
  1. **`GetLastError` is mandatory after a `CredDeleteW` FALSE return.** Microsoft conflates "didn't exist" with "real failure" by returning FALSE for both, so a TRUE-only check would shadow the idempotent revoke the Settings UI relies on.
  2. **`from_raw_parts` requires non-null even for length 0.** Guard with `if cred.credential_blob.is_null() || cred.credential_blob_size == 0` to avoid UB on a freshly-written credential whose blob pointer hasn't been allocated.
  3. **`CRED_PERSIST_SESSION` is wrong for OAuth tokens.** A credential with this flag is gone after logoff — fine for session-only secrets (the kind cwrap adapters carry), wrong for a long-lived refresh token.
  4. **The Rust blob format is implicit.** Buildmesh's parser (`services::opencode_oauth::parse_opencode_console_full_credential`) currently shapes a 5-field blob: `access_token` + `workspace_id` + `refresh_token` + `expires_at` + `server_id`. The first two are required for the live probe (see `parse_opencode_console_credential`); the last three ride along for `try_refresh` and the `X-Server-Id` header fallback. Any future extension that adds a sixth field must update both the writer (`services::opencode_oauth::persist_token_response`) AND the parsers atomically, or the live probe will silently drop the field (serde defaults — `skip_serializing_if = "Option::is_none"` on the writer + `#[serde(default)]` on the reader means both sides tolerate missing keys but never notice renamed ones). `parse_full_credential_round_trips_all_five_fields` pins the contract; CI's `git diff --exit-code src/types/generated/` is the wire-shape gate, but not the Rust-side wire-shape gate — keep both directories in sync by hand until both ts-rs exports and Rust struct shape are auto-derived.

## Coordinator Read & Drive API

Buildmesh exposes an **HTTP surface** for an external **Coordinator** (the user's remotely-hosted Hermes Agent first; a future in-app superagent second) to scan every Agent Node across every Mesh, drill into any one (the **read** half, below) and **drive** a chosen node (the write half — see *Coordinator Drive*). Plain JSON over the existing embedded HTTP server, **off by default**, behind separate capability-scoped tokens distinct from the mobile root token, bound to loopback + LAN (the user owns the remote tunnel — Tailscale / Cloudflare / WireGuard). Hermes is one instance of a Coordinator, not the category.

- **Architecture & rationale:** [`docs/adr/0008-coordinator-control-api.md`](adr/0008-coordinator-control-api.md). **Domain language:** *Coordinator*, *Node Digest* in [`CONTEXT.md`](../CONTEXT.md).
- **User guide (how to enable + consume):** [`docs/development/coordinator-read-api.md`](development/coordinator-read-api.md). **Spec:** issue #312.
- **Two endpoints (both authenticated with the read-scoped bearer token):**
  - `GET /nodes` — array of layered Node Digests. Spine is always present (lifecycle `status`, `needs_feedback` = `awaiting_input`, `waiting_since`, `last_activity`); the transcript-derived rich layer is present for the Claude Code family (Anthropic, Minimax, Kimi) or explicitly flagged `unavailable` (degrade-and-flag, never a silent omission). Cheap to poll.
  - `GET /nodes/{id}/log?tail=N` — on-demand raw recent turns (assistant text + tool calls) for one node. Content is **raw, not pre-summarised** — the Coordinator is itself an LLM. An unknown node id is a 404; every other degrade path is a 200 carrying a structured `unavailable` envelope.
- **Module layout (do not split):** `src-tauri/src/coordinator/` (`node_digest.rs` is the pure digest builder; `enrichment.rs` is the impure owner of provider capability + path resolution + bounded transcript read); `src-tauri/src/services/transcript_reader.rs` quarantines all Claude-Code JSONL shape brittleness; `src-tauri/src/http/routes/coordinator.rs` is a thin transport skin; `src-tauri/src/http/mod.rs` enforces the off-by-default + read-token gate in the dispatcher.
- **Contract test guard:** a real JSONL fixture in `src-tauri/src/services/transcript_reader.rs` test module makes a Claude Code shape change turn a local test red instead of silently degrading the Coordinator. This is the read-side form of the project's serde-default-fragility lesson.
- **Coordinator Drive (PRD #313, ADR-0008 §5–6; D1 #319 + D2 #320).** The write half: `POST /nodes/{id}/prompt` writes a prompt into a live node's PTY through #178's `AgentDriver` (`send_prompt` → `verify_delivery`) — the PTY's stdin *is* the input box, nothing is screen-scraped, and **any live node** is drivable (Claude Code queues stdin for a busy agent). Requires the **drive scope** (distinct from read, off by default, under the master kill-switch); the read token can never drive. The response carries an **honest verdict** — `Delivered` on a confirmed `awaiting → cleared` transition, else `Unverified` (queued-but-unconfirmed) — never success without confirmation. All the drive logic lives in `src-tauri/src/coordinator/drive.rs` behind two seams (`DriveTarget` = PTY/DB/events, `IdempotencyStore` = the ledger) so it is unit-testable without a real PTY or DB; `http/routes/coordinator.rs::prompt` is a thin skin. Do **not** grow a parallel write path — the scheduler (#178) reuses the same `AgentDriver`.
- **Idempotency (D2 #320).** Each drive carries a mandatory caller-supplied `idempotency_key`; the `coordinator_drive_prompts` ledger (`PRIMARY KEY (node_id, idempotency_key)`, `db/mod.rs`, schema v23) records the verdict once and a duplicate key **replays** it instead of re-sending, so a Coordinator's retry over a flaky network never lands a prompt twice (#178's cardinal rule). Load-bearing rules: the key is scoped **by node** (an accidentally-reused key still drives each node once, never leaks a verdict cross-node); the ledger row is written **only after a successful send** (a `NotLive`/`WriteFailed` drive leaves no row → retry re-attempts; but an `Unverified` send *is* recorded — re-sending it is the double-delivery #178 forbids); and lookup **fails safe, not open** — an unreadable ledger returns `503`, never a silent re-send (a read error must never be mistaken for "key never seen"). The pure `drive_node_idempotent(store, driver, …)` orchestrator encodes lookup → send → record and is tested against fakes (headline: same key twice = exactly one PTY write).
- **Tunnels are the user's job.** Buildmesh never opens an internet port — the threat model for the coordinator surface is "coordinating agent on a machine I control, reached over my own tunnel", not "autonomous agent on a public VPS reachable from the open internet". Reaching the read surface from outside the LAN is a deliberate user choice, not a Buildmesh feature.
- **Auth is two-tier and header-only (#500, ADR-0015).** Every request resolves to a [Role](../CONTEXT.md) — **Admin** (root token, the mobile `/api/*` surface) or **Coordinator** (read/drive tokens, `/nodes*`) — as **disjoint surfaces**: a token works only on its own surface (wrong-surface valid token → 403, no creds → 401). Role resolution lives in `src-tauri/src/http/auth.rs` (`authorize`/`guard`); the dispatcher calls `auth::guard(.., scope)` per route, and `/admin/*` is reserved Admin-only. Credentials travel only in `Authorization: Bearer` or the `bm_session` cookie — never `?token=`. The mobile shell/assets are public; the client logs in via `POST /api/session` (sets the cookie) and mints a single-use `?ticket=` per WebSocket via `POST /api/ws-ticket` (`src-tauri/src/http/ws_ticket.rs`).
- **Device Sessions are the per-phone Admin credential (#502, ADR-0018).** `POST /api/session` no longer echoes the root token into the cookie: presented the root token it *pairs* (mints a `device_sessions` row, returns a new per-device token in the JSON body); presented a known device token it *refreshes*. The phone persists the returned device token (web SPA → `localStorage`). `resolve_role` accepts a valid device token as `Role::Admin` and **never consults the IP** (roaming). Revocation deletes the row (blocks the next request) *and* fires the device id onto `http::revocation`'s broadcast channel; every live WS subscribes and force-closes on a matching id — the WS ticket carries its minting device id (`Option<i64>`; `None` = root token, unrevocable). Surfaced via desktop `commands::devices` ("Authorized Devices" panel) and remote `GET /admin/devices` + `POST /admin/devices/{id}/revoke` (`http/routes/admin.rs`), both over the shared `db` + `revocation` layer. The DB helpers are `_inner`-only (callers already hold the lock).

## LAN/VPN Exposure & Self-Signed TLS

The embedded server binds **loopback only by default** (#496). An off-by-default
**LAN / VPN Exposure** toggle (App Settings → `set_lan_exposure_enabled`, stored in
`app_settings.lan_exposure_enabled`) exposes it on the machine's interfaces; issue
#501, [`docs/adr/0017-opt-in-lan-exposure-and-self-signed-tls.md`](adr/0017-opt-in-lan-exposure-and-self-signed-tls.md).

- **Loopback stays plain HTTP; only non-loopback interface IPs get TLS** (`http::bind_specs`). This is deliberate: the attention webhook posts plain `http://localhost/api/attention/...`, so forcing TLS on loopback would silently break every agent's "awaiting input" signal. Do **not** "simplify" this to a single `0.0.0.0` TLS bind.
- **Self-signed cert** is generated with `rcgen` (ring backend), SANs cover `localhost` + loopback + interface IPs, and persisted as DER under `<app-data>/tls/` (delete that dir to rotate). `http::tls`.
- **The toggle rebinds live** — no app restart. `http::apply_binding`/`reapply_binding` signal a `watch` shutdown channel, await the accept-loop tasks (so the port frees), then bind afresh.
- **`MaybeTls`** (`http::stream`) is the single concrete stream type (`Plain(TcpStream)` | `Tls(Box<TlsStream<TcpStream>>)`) so route handlers stay non-generic; WSS rides the same enum. Route handlers take `&mut BufStream<MaybeTls>` — never re-introduce `BufStream<TcpStream>`.
- **Crypto provider is `ring`, selected explicitly** via `builder_with_provider` (no process-default; aws-lc-rs is not in the tree).

## Attention System

### How It Works
Agents signal they need user input via Claude Code hooks configured in `.claude/settings.local.json` (written by `inject_attention_hook` in `agent/spawn.rs`): a catch-all `Notification` hook (permission prompts, idle prompts, elicitations) plus a `Stop` hook (turn ended). Both run the same curl command, which forwards the hook's **stdin JSON** as the POST body (issue #878):

```
curl -sf -X POST -H "Content-Type: application/json" --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true
```

The hook reads `$BUILDMESH_PORT` (set per-agent in `spawn_environment`) at run time rather than baking a literal port, so it routes correctly across the 1992→1994 fallback and to the dev profile's 2992 when an agent is spawned by `buildmesh-dev`.

`POST /api/attention/{session_id}` (`http/routes/attention.rs`) classifies the payload and publishes a Node Turn (`node_turn::publish`), which:
1. Updates the DB status to `AwaitingInput` (`commands::attention::mark_attention` — status column is the single source of truth, there is no mirrored in-memory set)
2. Emits `attention-needed` (Tauri event + mobile WS fan-out)
3. Calls `session_naming::on_turn()` (triggers async LLM rename) and `autopilot::pipeline::on_turn()`

**No timer, polling, or debounce** — the event fires synchronously and immediately.

### False-Yield Suppression (issue #878)
Claude Code ends its turn when it launches background work (`run_in_background` Bash, timeout-backgrounded commands) and re-invokes itself when the `<task-notification>` arrives — so a Stop (or 60s-idle Notification) is *not* always "the user is needed". The route reads `transcript_path` from the hook payload and asks `transcript_reader::count_pending_background_tasks` for launched-but-unnotified task IDs (launch = a `tool_result` promising "You will be notified when it completes"; finish = a `<task-id>` notification with a **terminal** status — `running`-status notifications don't count). Pending work → the Node Turn is published via `publish_without_attention` (naming/autopilot still fire; no attention mark). Permission-prompt Notifications always mark, even mid-background-wait. Any unknown (empty/garbage body, unreadable transcript) degrades to marking — never to silence.

**Safety net:** `attention_autoclear.rs` arms on every mark; if the PTY then produces ≥512 bytes of output more than 3s after the mark with no user keystroke, the node flips back to `running` and `attention-cleared` is broadcast. The 3s grace absorbs the Stop-hook-vs-final-redraw race; the burst threshold ignores idle control-sequence trickle. This self-heals the cases the transcript scan can't see (hook-less providers, format drift, lost notifications). Every path that clears attention or accepts user input must call `attention_autoclear::disarm` (see `write_to_agent_blocking`, `http::ws`, `coordinator::drive`, `autopilot::pipeline`).

### Auto-Spawn Behavior
`AgentTerminal` component auto-spawns the agent when mounting an agent node with `status === 'idle'` and a `provider`. It uses `fitAddon.proposeDimensions()` to get PTY size before calling `spawn_agent`. This couples terminal mount directly to agent spawn — debugging attention issues requires tracing this path.

## Agent Node Management

### Agent Node ID Capture
Session IDs are **assigned, not captured**, for providers we control (Anthropic, OpenCode): the orchestrator mints a UUID up front, writes it to `agent_nodes.cli_session_id` *before* launch, and passes it to the CLI via `--session-id <uuid>` (`agent/spawn.rs`, `SessionIdMode::Assign`; ADR 0024). The PTY reader thread's `session-id:` / `session_id:` sniff (`session_capture.rs`) now runs **only** for *self-assigning* providers (Codex, Antigravity) where we don't control the ID — gated by `reader_should_capture_session_id` so there is exactly one writer per spawn (issue #651). Don't replicate either path — both are backend-only. `CLAUDE_CODE_SESSION_ID` is deliberately **not** used: Claude Code sets its `CLAUDE_CODE_*` vars *downward* into its own subprocesses, so a parent that spawns `claude` can't read it, and for Claude we already know the ID (we assigned it). See ADR 0024.

### Turn Counting and Node Naming
`session_naming.rs` captures PTY output and auto-names agent nodes via LLM summarisation (slug-based, e.g. `fix-auth-flow`). Buffering is gated: `on_output` only starts collecting after the first `on_turn` (first idle-prompt webhook) fires, so the Claude Code startup chrome — banner, "Bypass Permissions" warning, plugin/skill listing — is discarded before it can reach the LLM. The rename runs async one turn later, against clean post-startup content.

### Crash Recovery on Startup
`lib.rs:46-54` marks any agent nodes still showing `Running` status as `Suspended` during app startup, since a crash means no live process exists. These are then auto-resumed via `auto_resume_nodes` on the frontend's first draw.

### auto_resume_nodes
On app restart, the frontend calls `auto_resume_nodes` which iterates all `Suspended` agent nodes with a `cli_session_id` and calls `spawn_agent_inner` with `SessionIdMode::Resume`. Only Anthropic and Minimax (cwrap providers) are auto-resumed — Antigravity and OpenCode skip this path and go directly to `Idle`.

### Early-Exit Detection
The PTY reader thread records `spawned_at`. If the reader exits within 3 seconds, the agent node is marked `Error` and a `resume-failed` event is emitted. This catches failed `--resume` attempts where the agent CLI exits because the session has expired.

### `AgentNode.branch` is overloaded (base ref vs PR head ref)
The `branch` field on `AgentNode` (see Rust doc comment at `src-tauri/src/models/mod.rs`) means two different things depending on spawn source:

- **Issue-spawned, hand-spawned, and handover-spawned nodes** — `branch` holds the mesh's `base_ref` (resolved via `commands::git::get_default_branch`, typically `origin/main`).
- **PR-spawned nodes** (issue #420, where `source_pr.is_some()`) — `branch` holds the PR's `head_ref` instead. The worktree is cut from `origin/<head_ref>` (or `fork-<owner>/<head_ref>` for fork PRs, issue #443) so the agent lands on the same commits the PR is built from.

**Disambiguator:** `source_pr.is_some()`. When set, treat `branch` as the PR head ref; otherwise as the mesh's base ref.

**Canonical reader:** `spawn_agent_inner` in `src-tauri/src/agent/spawn.rs` derives the actual worktree `base_ref` from this field — see the `worktree_base_ref = if node.source_pr.is_some()` branch (around the PR-spawn fetch block). New code that needs the worktree's base ref should NOT reimplement the overload — call into `spawn_agent_inner`'s resolution or use the same `if source_pr.is_some()` pattern. The `commands::agent::create_pr_node` row-creation comment ("the *row* (`source_pr` is set, `branch` is the head ref) and in stage-2's `git fetch origin <head_ref>` worktree adoption") is the other half of the contract: the write side chooses the head ref precisely so the read side's `if source_pr.is_some()` switch lands on the right branch.

## Agent Process Architecture

### ProcessRegistry — Runtime State
Agent state lives in a **static** `ProcessRegistry`: `HashMap<i64, Arc<AgentProcess>>` using `once_cell::sync::Lazy`. The DB is **not** the source of truth for running agents — it's only used for `cli_session_id` persistence across restarts.

### AgentProcess Fields
Each entry holds:
- `child` — `Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>`
- `writer` — `Arc<Mutex<Box<dyn std::io::Write + Send>>`
- `master` — `Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>`
- `reader_alive` — `Arc<AtomicBool>` — set to `false` on PTY EOF; used to detect if an agent is still alive
- `job` — `Option<process_util::JobHandle>` — a Windows Job Object containing the agent's whole process tree (see *Killing the process tree* below); `None` on non-Windows or if assignment failed

The PTY handles are behind `Arc<Mutex<...>>` so the PTY reader thread and Tauri command handlers can both access them safely.

### Killing the process tree (Windows)
`kill_session` must kill **everything** the agent spawned, or a survivor pins the worktree's directory (as its CWD or via an open handle) and blocks removal on close. `taskkill /T` alone is insufficient: it walks *live* parent→child links, so it misses any descendant whose parent already exited — e.g. a dev server the agent backgrounded then orphaned. The fix is a **Job Object** (`process_util::JobHandle`): at spawn we assign the PTY shell to a kill-on-close job, so every process it later spawns is *contained* however it detaches. `kill_session` calls `TerminateJobObject` first (reaches detached/orphaned descendants), then keeps `taskkill /T` + `child.kill()` as fallbacks for the rare case job assignment failed. Assign happens immediately after spawn, before the shell launches the agent CLI, so the whole tree is covered. FFI to `kernel32` is declared inline via `extern "system"` (same no-new-deps pattern as `services::usage.rs`).

### Worktree Support (git2-based)
Buildmesh creates a dedicated worktree per agent node **itself**, via `git2` in `git/worktree/mod.rs` (`create_git_worktree` → `add_worktree_impl`) — for **all** providers, not just cwrap. This prevents concurrent agent node conflicts when multiple agent nodes target the same git repository. Two modes: `branched` (default, a real branch per worktree) and `detached` (a throwaway detached HEAD); both are cut from the configured Base Ref (default `origin/main`), resolved via `resolve_base_commit` with a fall-back to local `HEAD` when the ref is unresolvable (#230). See `docs/adr/0003-buildmesh-owns-worktree-creation.md` for why this moved off the agent CLI's old `-w` flag, and `docs/adr/0007-extract-git-module.md` for why the worktree lifecycle now lives in the `git` module.

**Worktree Provisioner** (`git/worktree/provision.rs`) is the four-branch decision that turns a Spawn Context into an on-disk worktree (`Reused` / `Adopted` / `Upgraded` / `Created`). The orchestrator (`agent/spawn.rs`) builds a `SpawnContext` from the mesh-row + node-row + optional warm-pool claim reads, hands it to `provision_for_spawn`, and matches the `ProvisionOutcome` to drive post-spawn bookkeeping (forget-after-spawn, name adoption, status writes). Base-ref resolution (`SpawnContext.base_ref`) reaches the cold-path `add_worktree_impl` end-to-end via this seam (pinned by `provision_for_spawn_cold_created_uses_spawn_context_base_ref_not_local_head` for issues #230, #248). `.worktreeinclude` is applied by `apply_worktree_include` (`git/worktree/mod.rs`) on every cold-path `Created` and warm-path `Upgraded` outcome — including **recursive directory copy** as of #248 (previously log-and-skipped; pinned by `apply_worktree_include_copies_directory_recursively`).

**Resume:** worktrees are created only when the directory does not already exist (the `if !host_path.exists()` guard in `spawn.rs`), so resume simply re-spawns inside the existing worktree — no re-creation, and none of the old `-w` "already checked out" failures.

**Auto-sync on spawn (issue #213, relaxed by ADR 0020):** before creating a *new* worktree (resume doesn't re-sync), `git::sync::fetch_origin` runs `git fetch <remote>` + `git pull --ff-only --no-rebase` on the parent mesh. The `--no-rebase` is required to defeat a global `pull.rebase=true` config — a rebase on a diverged history would write conflict markers to the working tree, silently mutating the user's local branch on what's supposed to be a read-only step. The sync is best-effort: dirty parents, no-origin repos, and already-up-to-date branches are silent; a fetch failure, diverged history, or unreadable repo surfaces a `mesh-sync-warning` toast (frontend label: `Sync`) and spawn proceeds from local HEAD. See `docs/adr/0001-auto-sync-mesh-on-node-spawn.md` and [[buildmesh-pull-rebase-default]].

**Spawn-time fetch TTL + background mesh sync (ADR 0020):** the spawn-time auto-sync is SKIPPED when the mesh was successfully synced within `services::fetch_freshness::SPAWN_FETCH_TTL` (5 min) — the background worker (`services::pool_worker`) re-fetches every idle worktree-enabled mesh once per `BACKGROUND_SYNC_INTERVAL` (3 min, gated on last *attempt* so an offline machine doesn't hammer retries) and triggers `warm_pool::on_fetch_completed` when the ref advances, so both the mesh and its warm pool stay continuously fresh without a network round-trip on the click-to-terminal path. All fetch paths stamp the registry via the `locked_*` wrappers in `git::sync`; the PR-head fetch is never skipped (correctness, not freshness); the manual Sync command is the "latest right now" override. Do NOT add a new fetch call site that bypasses `locked_fetch_origin` / `locked_do_sync` — it would fetch without stamping freshness and reintroduce redundant spawn-time fetches.

**Warm pool default (schema v24, ADR 0020):** `pre_spawn_pool_size` defaults to `1` (pool ON) for new meshes, with a one-time flag-gated backfill (`pool_default_backfill_v24`) for existing worktree-enabled meshes. Opt out per mesh via the Worktrees Probe.

**Close/removal (optimistic + deferred):** closing a node is split in two. Phase 1 (`services::agent_node::delete`) kills the process tree (via the Job Object — see *Killing the process tree* above, so a dev server the agent spawned can't keep the directory pinned) and, in one transaction, deletes the `agent_nodes` row *and* enqueues the worktree into `pending_worktree_removals` — fast and authoritative, so the UI drops the node at once. The slow recursive directory delete (`remove_one_worktree`) runs as a background drain (`process_pending_removals`) that dequeues only on success; an app quit mid-cleanup is resumed by the startup reconcile in `lib.rs` `setup()`. Net: "node gone from UI" no longer implies "directory gone" — close is eventually-consistent on disk, and a stuck removal raises a `worktree-cleanup-failed` toast. See `docs/adr/0004-optimistic-node-close-deferred-worktree-removal.md`.

## Autopilot Mode (PRD #480, slices #481–#485)

Event-driven agent provisioning: a mesh with `autopilot_enabled` is polled every 2 minutes (`services::autopilot::start_autopilot_worker`, started in `lib.rs` setup) for open GitHub issues carrying the mesh's trigger label (default `buildmesh:run`). New issues — capacity-gated by `autopilot_concurrency_limit`, deduped against `db::list_known_autopilot_issue_numbers`, and collaborator-gated via `autopilot::gate_trigger` (ADR-0012 §5) — spawn ordinary two-stage issue nodes with `use_worktree` forced on and `worktree_mode` forced `branched` (enforced in `spawn_agent_inner` off the `autopilot_runs` ledger row).

- **Ledger, not a node column:** auto-spawned nodes are marked by a row in `autopilot_runs` (`node_id` PK, cascade-deleted). It carries the wrap-up state machine: `implementing → finishing(attempts) → completed | failed`. Do NOT add an `autopilot` column to `agent_nodes` — the positional `AGENT_NODE_COLUMNS` projection and its JOIN consumers are the reason the satellite table exists.
- **Turn evaluation:** `node_turn::publish` fans out to `autopilot::pipeline::on_turn` (third consumer). For piloted nodes (in-memory registry in `autopilot::evaluator`, hydrated from the ledger at startup) a worker thread classifies the recent PTY tail via `claude --print` (COMPLETED / BLOCKED / WORKING; every failure degrades to WORKING). The evaluator env routes through the mesh's `autopilot_provider` side-channel (`naming_backend_env`), never the node's own model.
- **Wrap-up (ADR-0011):** on COMPLETED, the user-customizable `<app-data>/autopilot/finish.md` template (seeded on first use; `{{PR_STEP}}`/`{{ISSUE_REF}}` placeholders) is injected into the PTY (bracketed-paste for multi-line). The *agent* runs tests/commits/pushes/`gh pr create`; Buildmesh then verifies deterministically (worktree clean + branch pushed + open PR unless policy `none`) and either completes the node (`SessionStatus::Completed`), injects a correction prompt (max 3 attempts total), or fails it (`Error` + `autopilot-finish-failed` event).
- **Prompt injection is two-phase — never glue Enter onto a paste (#874):** ink-based TUIs (Claude Code) batch stdin reads, so a `\r` in the same write as a bracketed paste is absorbed into the paste and the prompt sits staged, unsubmitted (node 2328, 2026-07-17). `pipeline::write_prompt_to_pty` stages the paste alone, then a background watcher waits for the echo + output quiescence, sends `\r` as its own write, and verifies PTY output follows (`press_enter_until_output`, bounded retries; final failure marks the node for attention instead of stalling silently). The launch watcher submits prefills through the same helper.
- **Turn delivery is best-effort; the poller backstops it (#874):** the attention callback is an HTTP hook that can silently fail, so lost turns must be recoverable. Three layers: (1) the in-flight guard *queues* turns arriving mid-evaluation and re-runs (`try_begin_evaluation`/`end_evaluation_and_check_rerun`) — never drops them; (2) the green-only re-drive completes observably-done `finishing` rows stale ≥5 min; (3) `pipeline::watchdog_pass` synthesises a full turn evaluation for any piloted node with output no evaluation reacted to (`evaluator::note_evaluation` vs `LAST_OUTPUT`) once it has been quiet ≥3 min — classify in `implementing`, verify/correct in `finishing`.
- **Manual entry point:** `trigger_finish` command (header ✓ button) enrolls any live node in the same machine at `finishing`.
- Frontend events: `autopilot-blocked` / `autopilot-pr-created` / `autopilot-finish-failed` → toast stack in `App.tsx`; config UI lives in `MeshPropertiesTab` (one atomic `update_mesh_autopilot` write).

## Logging and Crash Handling

- Logs written to `buildmesh.log` via `tracing-appender` (not console)
- Panic hook writes to `logs/panic.log` with thread name, thread ID, and full backtrace
- **`RUST_BACKTRACE=1` is the launcher's job.** `Backtrace::capture()` (lib.rs:364) reads the env var at runtime and returns the "disabled backtrace" placeholder when it's unset. All four launchers (`scripts/run.ps1`, `scripts/run-dev.ps1`, `scripts/run.sh`, `scripts/run-dev.sh`) set it before launching; `tests/unit/launch-script-backtrace.test.ts` pins the contract so a refactor can't silently drop the env var. Without it, the "full backtrace" bullet above is a lie — the file would have one placeholder line.
- **`panic.log` vs `panic_early.log`** — two hooks, two files (`lib.rs:41-128` + `lib.rs:348-382`). The early hook is installed in `run()` BEFORE Tauri setup so it catches panics during Tauri-init that the main hook (installed later in `setup()`) can't. Bundle-id is derived from the binary name (`buildmesh-dev.exe` → `com.alond.buildmesh.dev`), so dev-profile crashes don't pollute the stable hub's logs. Both hooks `flush()` + `sync_all()` because `panic = "abort"` kills the process via `__fastfail` before the OS file buffer flushes.
- **`panic.log` is invisible to `buildmesh.log` pattern scanning.** The main panic hook writes to the file + `eprintln!`s but never pushes to the tracing pipeline. `/verify`'s full-tier log-scan (issue #158) tails `panic.log` and `panic_early.log` separately and treats any new line as an unconditional fail; the `scripts/run-dev.ps1` and `scripts/run-dev.sh` launchers also fast-fail on the same condition so a panic-only crash can't masquerade as a successful launch.
- `_guard` from `tracing_appender::non_blocking` is leaked with `Box::leak` to live for app lifetime — dropping it would stop logging

## Environment Detection

- `env_for_path` — heuristics: `/mnt/`, `/home/`, `\\wsl$`, or `/` → WSL; everything else → Windows
- `to_host_path` — converts Linux paths to Windows UNC (`\\wsl$\Ubuntu\home\user`) for Windows-side file operations on WSL sessions