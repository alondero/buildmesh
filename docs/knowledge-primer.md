# Buildmesh — AI Context

> **Reading this file:** it is ~150 KB. Do not read it whole. List the sections with `rg -n "^#{2,3} " docs/knowledge-primer.md`, then read only the sections for the code you will change, and verify each against its owning module (the code wins on any disagreement).

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

## Key Conventions

### First-class Model Providers and the credential-per-row invariant

A **First-class Model Provider** is one Model Provider Buildmesh ships built-in
knowledge of — brand identity (icon, accent colour), billing model, and a Usage
Meter fetcher. Examples: Anthropic, MiniMax, Kimi (Moonshot). Rendered as one
card on the Providers page; polled by one Usage Meter fetcher. See CONTEXT.md
"First-class Model Provider" for the canonical definition.

**The invariant — one credential/billing identity per row.** Per CONTEXT.md,
"Usage follows the credential, not the pairing" — proxying one credential
through several harnesses is still one Usage Meter (a single Moonshot API key
used via Claude Code and via Codex is one wallet). A provider MAY legitimately
expose multiple Usage Meters (e.g. an Anthropic subscription *and* an API
wallet — different billing relationships, same brand). What is forbidden is
*duplicate rows for the same credential/billing identity* — that produces
two cards on the Providers page, two fetcher paths, and confusing UI.

**Usage Meter contract.** Rust owns the additive wire model in
`services/usage/types.rs`; `cargo test` generates its TypeScript consumers.
`ProviderUsage.windows` and `.balance` remain the compatibility path for
rolling percentage quotas and wallets. New adapters may additionally provide
a verbatim `plan` label and explicit `meters`: `metered` carries a
`UsageAmount` (used, optional limit/remaining, unit, optional percentage and
reset), `no_individual_limit` carries the same amount without inventing a
limit, and `unlimited`, `managed_externally`, and `unavailable` distinguish
valid non-percentage states. The UI renders zero as zero; only an absent legacy
percentage or an explicit unavailable state is labelled "Unavailable". The Codex
adapter maps ChatGPT `plan_type`, rolling windows, top-level extra rate limits,
credit balance, and `spend_control.individual_limit` into that contract; a null
`rate_limit` is a valid Business/Enterprise snapshot rather than a parse error.
Muse Code (`muse-code`) reads the harness OAuth credential and calls Meta's
`POST /muse-code/key` reconciliation endpoint. Its `subs_usage.window` and
`subs_usage.weekly` supply percentages and Unix-second reset times; the plan
label comes from `subs_tier_name`. Windows resolves credentials inside the
WSL login environment, honoring `MUSE_AUTH_PATH` and `XDG_CONFIG_HOME`.
Never derive remaining allowance from local requests or MSP token/context
events, and never fold it into a Meta Model API pay-as-you-go wallet.

**Observed Session Telemetry is not a Usage Meter.** Muse MSP
`session/tokenUsage` and `session/contextUsage` notifications are ingested by
`agent/provider/muse/telemetry.rs`, stored per Agent Node, and surfaced on
Node Detail plus the Node Digest `observed_session_telemetry` field. The
payload is labelled `kind: observed_session_telemetry`. Do not route it
through `UsageAdapter`, `get_provider_meters`, or the Usage probe tab, and
do not derive remaining quota, reset time, or dollar spend from it. Cache
read/write counters stay separate from counted-once `prompt_tokens`. The
Muse lifecycle task should call `telemetry::ingest_line` for every
`MspTransport::events()` notification once that transport exists.

**Usage page links.** Each `UsageAdapter` owns an optional `usage_page` for
its billing identity: a `UsagePage` URL and label that distinguish dashboards
from console, guide, or subscription fallbacks. `assemble_meters` attaches it as
additive `ProviderMeters.usagePage`, including disabled, failed, and remembered readings;
URLs are metadata, not stored in the reading caches. The catalog omits the link
when a reading reports `managed_externally`, and unknown providers have none.
The Usage panel renders it through `SafeLink` to open the system browser, as
an icon-only `↗` link in the row's header beside the provider name rather than a
labelled link of its own, so a meter's height does not grow with the destination.
The label travels in the `ariaLabel` and hover `title` instead of visible text.

The icon-only form suits a row that already shows the provider name, but it
shrinks the hit target and drops the visible cue the labelled call sites carry
(`PrPill`, the Git Issues / PRs tab headers); prefer a labelled link where the
surrounding row does not already name the destination.
Provider destinations and first-party evidence are recorded in
[the usage-page research](research/provider-usage-pages.md).

**Usage cache identity.** The five-minute cache is keyed by provider plus an
opaque account/authentication-source fingerprint selected through the
`UsageAdapter` seam. Keyed adapters receive a process-salted SHA-256
credential fingerprint automatically; native adapters override the identity
when their provider can select OAuth, cloud, workspace, or other credential
sources. Fingerprints keep tokens, keys, account identifiers, and credential
paths out of cache keys and logs. A repeated identity keeps the existing TTL
hit; changing account or auth source starts a distinct entry.

**Usage Meter last-known fallback.** A second, *durable* tier
(`services/usage/last_known.rs`) keeps the last reading each provider actually
reported in `<app_data_dir>/usage_last_known.json`, for seven days from the last
successful fetch — it survives restarts, unlike the five-minute in-process cache.
When a fetch cannot produce a reading because no usable credential is available
(a harness not signed into yet today, an expired native token),
`assemble_meters` serves that remembered reading instead of hiding the row and
stamps `ProviderMeters.cachedAt` (epoch seconds, `null` for a live fetch) so the
Usage tab can render "Last known value · …" plus a `Cached` header badge with
the fetch instant on hover. A transient failure (rate limit, transport error, or
a provider answering without usable quota) falls back the same way while a
reading is remembered. It is keyed by **provider id**, not
the identity fingerprint (which is process-salted, and for Muse *is* the rotating
access token). It never replaces a live reading or a
rejected-key prompt; with nothing remembered the row is hidden (or errors) as before. See
[ADR-0037](adr/0037-usage-last-known-fallback.md).

**Claude Code authentication source.** The Anthropic meter follows Claude's
documented credential precedence rather than always reading
`~/.claude/.credentials.json`. Cloud flags (`CLAUDE_CODE_USE_BEDROCK`,
`CLAUDE_CODE_USE_VERTEX`, `CLAUDE_CODE_USE_FOUNDRY`) from the process
environment or the user `settings.json` `env` block report
`managed_externally` for AWS Bedrock, Google Vertex AI, or Microsoft Foundry
and must not present a dormant OAuth login as active. Environment API keys,
bearer tokens, and `apiKeyHelper` outrank stored OAuth. `CLAUDE_CODE_OAUTH_TOKEN`
uses that token for the request and derives plan from usage-body plan fields or
the token's own `/api/oauth/profile`, never from a dormant local login.
**Exception — `claude setup-token`:** those tokens are model-request-only
(`user:inference`) and commonly lack `user:profile`. Anthropic's `/usage` and
`/profile` endpoints then return a scope/`permission_error` rather than a
usable plan. Buildmesh keeps the account logged in, surfaces an explicit
scope limitation (not "login expired"), and does not invent a plan name.
Full `/login` OAuth (or a token that includes `user:profile`) is required for
Enterprise plan + spend together. This is an intentional exception to the
"Enterprise OAuth accounts show plan and spend" acceptance criterion for
inference-only env tokens. Named Anthropic profiles
are mode-aware: `user_oauth` uses the profile credential for usage, while
`oidc_federation` (named, active, or env-configured) reports
`managed_externally`. Env federation requires the full WIF set (rule, org,
service account, and identity token/`_FILE`); a partial pair does not suppress
stored OAuth. A set-but-empty `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN`
still occupies that credential slot. An active `user_oauth` profile ranks
below a *working* `/login` credential (expired tokens and fetch 401/403 fall
through / retry) and above a missing login. `user:profile` scope failures are HTTP 403 with the explicit scope message:
env tokens mention `setup-token`, Claude Code `/login` credentials guide
`/login`, and named `ANTHROPIC_PROFILE` credentials guide
`ant auth login --profile <actual-name>` (because `/login` cannot repair that
higher-priority profile). Non-scope 401/403 auth failures are likewise
origin-aware: env tokens guide refreshing or unsetting
`CLAUDE_CODE_OAUTH_TOKEN`, profiles guide `ant auth login --profile
<actual-name>`, and `/login` store credentials guide `/login`. HTTP 401
remains an expired/revoked credential even if the body mentions scopes. Native OAuth reads the platform store (macOS
Keychain service `Claude Code-credentials`, suffixed from `CLAUDE_CONFIG_DIR`,
with `.credentials.json` as fallback when Keychain is missing or unusable) and
queries `GET /api/oauth/usage` directly — never by spawning the Claude CLI.
Consumer plans keep five-hour and seven-day windows; Enterprise prefers the
`spend` object and falls back to `extra_usage`.

**The Spawn Menu is where harness↔provider pairings live.** Its backend list
contains one Spawn Option per **stored** `(harness, provider)` pairing as the
composite id `<harness>:<provider>` (e.g. `claude:kimi`). Pairings are *not*
rows in `BUILTIN_PROVIDER_ACCOUNTS` — they live in
`AppPreferences::provider_pairings` and `effective_pairings` returns stored
rows only (ADR-0025: no auto-derived Claude pairing on key alone). Endpoint
URL + model tiers live on the pairing (Harnesses page), not the account.
The desktop spawn picker renders harness parents and puts their saved Launch
Configurations in each harness submenu — only user-saved recipes appear, so a
fresh install shows none. The backend also retains
the pairing rows for selectors that still choose a provider route directly.

**The registries are independent.** The brand string may coincide across
namespaces, but each registry is the single source of its own kind:

| Registry | What it carries | Example for Kimi |
|---|---|---|
| `BUILTIN_PROVIDER_ACCOUNTS` (`src-tauri/src/preferences/resolver/catalog.rs`) | One row per credential/billing identity. Self-auth rows always appear via `default_provider_accounts`; keyed first-class (`self_auth: false`) are catalog-only until added (`keyed_first_class_catalog`) — credentials + Usage Meter only. | `id: "kimi", self_auth: false` (endpoint on pairing / `first_class_surfaces`) |
| `HarnessProfile` + `Provider::Kimi` enum variant + `KIMI` adapter | The Kimi Code CLI Agent Harness — uses `~/.kimi/config.toml` for its own auth; Buildmesh doesn't manage the credential. | `HarnessProfile { id: "kimi", harness: "kimi", binaries: &["kimi"] }` + `KimiAdapter` |
| `src-tauri/src/services/usage/catalog.rs` | One row per fetchable Usage Meter: provider id, native-harness visibility gate or account-key lookup, and fetch adapter. Add a meter here instead of creating parallel lists or dispatch matches in `commands/usage.rs`. | `UsageMeterDefinition::keyed("kimi", usage::kimi_usage)` |

The string `"kimi"` appearing in both is fine because the namespaces are
different (`ProviderAccount.id` vs `HarnessProfile.id` / `Provider` enum).
What is **never** fine is two rows in `BUILTIN_PROVIDER_ACCOUNTS` for the same
credential — that produces two cards, two fetcher paths, and confusing UI.

**Pinned by tests** (regression net):
- `builtin_provider_accounts_have_no_via_substring_in_id` (`src-tauri/src/preferences/tests/catalog_tests.rs`) — any id with `"via"` is a pairing shorthand and must be expressed as a composite Spawn Option (`claude:kimi`), not as a separate row. A mechanical guard against the specific class of bug PR #1044 introduced.
- `kimi_via_claude_id_does_not_exist_in_default_provider_accounts` — the literal dual-id bug.
- `kimi_is_first_class_claude_compatible_with_moonshot_endpoint` — catalog + `first_class_surfaces` shape (not in defaults).
- `provider_accounts_migrates_stored_kimi_via_claude_into_first_class_kimi` — one-time migration for users who picked up PR #1044.
- `default_provider_accounts_are_self_auth_only` / `effective_pairings_stored_only_no_auto_derive` / `migrate_legacy_account_endpoint_into_claude_pairing` — ADR-0025.

**Don't.** Do not add a `kimi-via-claude`-style companion row when restoring
or re-introducing a First-class Model Provider. Keep it in the keyed catalog
(`self_auth: false`) and let the Harnesses page attach pairings explicitly.
If a credential migration is needed (e.g. a user already stored a key against
the companion id), carry it over in a one-time read migration that persists
back to `preferences.json` — don't leave stale data.

**One-shot migration flag.** A read-migration that *auto-derives* state
(ADR-0025: pairing rows for legacy keyed accounts with no Claude attach) must
be gated on a persisted boolean so it runs exactly once per install. Pattern
in `preferences.rs`: `ad0025_account_pairings_migrated: bool` on
`AppPreferences` (`#[serde(default)]` so older installs load as `false`),
set inside `migrate_prefs_json`, then a re-deserialise gate (`serde_json::from_value`
returning `Err` ⇒ keep the on-disk file intact rather than `unwrap_or_default()`,
which previously overwrote a partially-unknown prefs file with defaults — a
silent data-loss path).

**A read failure is not a licence to write defaults.** `preferences.json` holds
credentials, so an unreadable file must never be replaced by
`AppPreferences::default()`. `preferences::storage` owns that rule: it classifies
the file into `LoadState::{Missing, Healthy, Corrupt}` on every read, publishes
only `Missing` and `Healthy` to the writable cache, and re-checks the file on
every write rather than latching a flag that can disagree with the disk. A
corrupt read still *serves* defaults so read-only callers (spawn routing, the
circuit classifier, the usage panel) keep working; the defaults simply never
reach the disk. `preferences::recovery` owns the bytes: `classify` is pure and
returns a content-free `CorruptionInfo` (a corrupt file holds plaintext API
keys, so no diagnostic may echo a value), every successful write refreshes an
owner-only `preferences.json.bak`, and `restore_backup` / `reset_to_defaults`
archive the current bytes before replacing them. Surface the state as a
*successful* `get_preferences_health` call — never as an `Err` string the UI
has to pattern-match, and never as a `failed` resource status, because the
read itself did succeed.
**The Settings Harnesses pane does not depend on the Spawn Menu — and neither
does its retry.** The attach picker ("Add proxied provider") resolves its whole
`harness_id → compatible accounts` map in one backend call,
`preferences::compatible_providers_by_harness` — keyed by the union of the
effective harness profiles, the harness ids named by stored pairings, and the
harness half of every saved Launch Configuration's spawn option, so it covers
every row the pane can render. `loadPairings` therefore takes no providers list,
and the modal's mount fan-out loads pairings alongside every other resource
(issue #1935). Before that, pairings took its harness ids from `list_providers`
and issued one `compatible_providers_for_harness` per harness, which both cost N
round trips and made the pane open in `providers + pairings` — the Codex install
probe behind `list_providers` could take seconds. Don't reintroduce the
dependency.

`retryResource` in `useSettingsResources` takes no cross-resource preconditions:
every loader reads what it needs, so a retry is just the loader again. The
`retryResource('pairings')` boundary check from #1534 round 4 ("Awaiting providers
list — retry providers first") is **gone** and must not come back. It required a
providers list that no call site ever passed, so Retry on a failed pairings load
re-read nothing and replaced the real error with an unrecoverable message — and
since providers had usually succeeded, the user had no providers banner to be
pointed at and could only recover by reopening the modal. Its original
justification was the provider-menu dependency, so removing that dependency
removed it. When providers genuinely fails, the Harnesses pane renders the
providers banner ahead of the pairings one; that ordering is the affordance the
check was standing in for.

### Terminal Persistence (CRITICAL)
Startup identity recovery (#1555) lives in `services/session_recovery.rs`. List all Suspended nodes before recovery, including NULL/empty CLI session IDs; never restore the old SQL filter or one-time Codex migration gate. Historic recovery requires an unambiguous workspace/time match and a conditional write against the original fresh-start timestamp. Fresh intent atomically clears identity and records that timestamp; Resume preserves it. Missing-ID legacy nodes may be approval-gated and must not be auto-started by transcript matching. See [node-resume-recovery.md](learning/node-resume-recovery.md) for provider evidence, limits, and test coverage.

`TerminalManager` is a **singleton**. xterm.js instances survive React remounts via a hidden container stack. Never call `dispose()` on a terminal unless the agent node is explicitly deleted — see `src/components/Terminal/Terminal.tsx`. Disposing a terminal causes permanent blanking.

### xterm + addon lazy-load (issue #1568)
`@xterm/xterm` and the five `@xterm/addon-*` packages add ~430 kB minified / ~100 kB gzip to the desktop entry chunk if statically imported — too big to pay for users who never open a canvas. `TerminalRegistry.doCreate` and `BuildRunTerminalRegistry.doCreate` keep the runtime imports behind `await import(...)` inside the function body and use `import type { Terminal }` (and the other addon classes) at module scope, so Rollup erases the types and the dynamic imports emit a separate chunk fetched only when a terminal pane is first attached. `WebglRendererPool.activate` dynamic-imports `loadWebglRenderer` (which carries `@xterm/addon-webgl`); the pool reserves the LRU slot synchronously and schedules the renderer attach inside the awaited promise. **Do not** revert to static `@xterm/xterm` imports — see the budget cap at `scripts/bundle-budget.json`; the entry chunk's ceiling is enforced by `scripts/check-bundle-size.mjs` and CI. Two pre-existing Vite warnings remain (`Terminal.tsx` and `agentNodeStore.ts` are both statically and dynamically imported) because the dynamic imports in `meshStore.deleteMesh` are load-bearing workarounds for the `meshStore → agentNodeStore → Terminal.tsx → uiStore → meshStore` ESM cycle (`src/stores/meshStore.ts:5`); clearing them requires restructuring the store layer, which is out of scope for the cold-only-surface mandate.

### Command Code keyboard (Ink 7)
Command Code's Ink TUI can swallow keys when xterm.js leaves an unmatched bracketed-paste wrapper, or when the CLI's kitty-keyboard probe races ConPTY. Agent terminals for the `commandcode` harness set xterm.js `ignoreBracketedPasteMode`; other harnesses keep bracketed paste so a multi-line paste stays one prompt. Spawn sets `TERM_PROGRAM=vscode` so Command Code skips the probe. Every agent PTY is a real terminal: `wrap()` drops inherited `NO_COLOR` / `TERM=dumb` / `FORCE_COLOR=0` and sets `TERM=xterm-256color`, `COLORTERM=truecolor`, and `FORCE_COLOR=3` (and puts those keys on `WSLENV`).

### PTY input

Each `write_to_agent` call enqueues one buffer. The per-agent writer thread drains that buffer with a single `write_all`. xterm's paste is one data event, so a large or multi-line paste stays one write, including its bracketed-paste markers. Do not split or pace that write to work around a slow provider. The multi-second stall on a large Windows paste is the provider reading console input one record at a time; evidence and the upstream reader change are in [Large paste latency](learning/large-paste-latency.md).

Desktop clipboard paste into native Windows Grok uses its Ctrl+V command
instead of streaming clipboard text. `TerminalRegistry.pasteClipboard` owns
keyboard and context-menu delivery; its persistent element captures browser
paste before xterm. `Terminal` selects this policy only for Grok on a Windows
host with a Windows node. WSL and mobile/remote paste must send their own text,
since they do not share the desktop clipboard. Programmatic `term.paste(text)`
and backend prompt injection retain their existing semantics. See the
[Grok paste investigation](learning/grok-terminal-paste.md) for runtime evidence.

### PTY output streaming (issue #1385 / #1393)

Windows builds ship a pinned Microsoft ConPTY DLL and its matching native console
hosts. `build.rs` runs `scripts/prepare-conpty.mjs` to verify the NuGet package
checksum and stage the runtime beside Cargo binaries (including test binaries);
`tauri.windows.conf.json` packages the same files beside the installed executable.
The first Windows build needs Node.js and access to NuGet; subsequent builds use
the verified archive under `src-tauri/target/conpty/`. The sandbox's owned ConPTY
links the bundled DLL's Create/Resize/Close exports, while `portable-pty` loads
the same DLL for ordinary terminals. Keep the DLL and host versions together.
The inbox Windows console can forward DEC 2026 frame-end markers before flushing
its rendered text and cursor restoration, exposing intermediate cursor positions
in xterm. Do not compensate by disabling harness animations or delaying all PTY
output. Live Windows frame-order tests cover both PTY creation paths.

The PTY reader thread still sees every OS `read()` (session-id capture, auto-naming, autopilot). A sibling batcher coalesces those slices (8 ms window or 32 KiB = four 8 KiB PTY fills) and pushes **raw bytes** over a per-session Tauri `Channel`. The sink type lives in `pty::sink` (`OutputSink` / `OutputSinks`). Agent terminals use `pty::sink::AGENT` via `subscribe_agent_output` / `unsubscribe_agent_output` (`agent::output`). Build/Run terminals use a **sibling map** `pty::sink::BUILD_RUN` via `subscribe_build_run_output` / `unsubscribe_build_run_output` — both surfaces key by the same node id, so sharing a map would paint agent bytes into a Build/Run xterm. Bytes that arrive before subscribe are buffered on `OutputSink` and flushed in order — never mixed with the JSON event (those two IPC paths have no ordering). `agent-output` `line` and `build-run-output-{sessionId}` are test injection only. `pty::batch::with_batcher` drops the producer before join (otherwise the reader deadlocks on EOF). The Channel subscription belongs to the persistent terminal, not to one process incarnation: stale-process cleanup, PTY EOF, retry, resume, regenerate, and Build/Run `close_build_run` must preserve it. Agent Node deletion and explicit terminal disposal unregister it. Don't put production PTY bytes back on the JSON event.

The first-spawn path is the load-bearing one. The frontend subscribes as soon as the xterm exists, then `prepare_context` calls `kill_agent` even when there is no process. Unregistering there drops the Channel; the new reader's `ensure()` creates a disconnected pending sink and the viewport shows a cursor with no text. `kill_session` / the PTY-reader epilogue / `close_build_run` must not unregister. Pins: `kill_session_without_process_preserves_output_subscription`, `replacement_reader_reuses_the_live_channel`, `process_lifecycle_does_not_unregister_node_output_subscription`, `agent_and_build_run_maps_do_not_cross_talk`. Frontend: subscribe on `TerminalRegistry` / `BuildRunTerminalRegistry` create, unsubscribe only on `dispose` (never on remount, auto-spawn, detach, or `getOrCreate` reuse).

Tauri 2.11's raw Channel transport has a payload-shape boundary: frames smaller than 1 KiB reach JavaScript directly as an `ArrayBuffer`, while frames at or above 1 KiB use the fetch path and arrive as a `Response`. `subscribeAgentOutput` / `subscribeBuildRunOutput` (shared `subscribeRawPtyOutput`) must consume `Response.arrayBuffer()` asynchronously and serialize those reads with later frames so terminal bytes cannot overtake each other. The boundary-to-xterm regression is pinned in `tests/integration/agent-terminal-auto-spawn.test.tsx` and `tests/unit/build-run-terminal-persistence.test.tsx`.

Terminal container resize is coalesced by the shared `TerminalResizeScheduler`, used by both agent and build/run registries. It waits for 200 ms without a size change before fitting, so pane dragging causes one PTY resize after the drag settles. A horizontal xterm resize reflows normal-buffer scrollback, and full-screen TUIs redraw for the final PTY size. Codex uses its fullscreen transcript mode rather than inline scrollback, avoiding a transcript clear and replay on width changes; history navigation belongs to Codex while it is running. Keep DOM measurement on the next animation frame and do not restore per-observation or per-frame `fit()` calls.

### Canvas Layout (View Modes)
The canvas exposes five **View Modes** (wayfinder #982; state model #983; rendering #986; Filtered added #1609). The active mode is a pure UI string-literal union — no backend serialises it. The grid render, keyboard traversal (#987), and unit tests share one visibility definition, written as pure helpers so the mode→node-set mapping is testable without spinning up the store:

- **single** — Solo the active node; subsumes the old maximize toggle. Escape returns to the grid mode `single` was entered from.
- **mesh** — Scope to the sidebar-selected mesh. With no selection, falls back to the active node's mesh, then the first loaded mesh.
- **pinned** — Cross-mesh filter over `is_pinned`; deliberately never touches `selectedMeshId`.
- **all** — Every loaded node, across every mesh. All Nodes carries a one-way invariant: `viewMode === 'all'` requires `selectedMeshId === null`. `setViewMode` enforces this transition directly so callers remain side-effect-free.
- **filtered** — Cross-mesh view narrowed by the Grid Controls (free-text search + provider/status filters; the Search Nodes bar mounts in the title bar only while this mode is active).

The five-segment control is the bespoke `ViewModeSwitcher` in the title bar.

### Probe Context Lenses (issue #1456)
Probe destinations have explicit ownership in `src/lib/probeContext.ts`; the
complete `PROBE_TAB_DEFINITIONS` record is the source of truth for ownership
lens,
baseline, selection-following, pinning, and statefulness. The three lenses are
`Host` (machine-wide provider/account/runtime state), `Mesh` (one repository,
its configuration, GitHub feeds, worktrees, automation, and notes), and
`Agent` (one Agent Node's changes and actions). Agent History is a Host finder
with an explicit repository filter and a separate discovered-session source. Usage is
Host-lens and must never display or infer a Mesh name. Project Files is
Mesh-owned but may show a focused Agent Node's working tree; Agent Changes is
Agent-lens and uses the node-base baseline. The `useProbeContext` hook is the
read seam for the shell and destination tabs: use its subject, resolved IDs,
paths, and `hasRequiredContext` instead of reconstructing ownership from
`selectedMeshId` or `activeNodeId`. The Probe header labels the subject and
whether it is following selection or pinned. Pins are session UI state keyed to
the destination; a missing pinned subject renders an explicit empty state and
must not fall back to a newly selected Mesh or Agent Node. See
[`docs/adr/0029-probe-context-lenses.md`](adr/0029-probe-context-lenses.md)
for the destination mapping and mixed-ownership decisions. Issue #1375 moved
Probe navigation title-bar-first — a command palette plus an on-demand
inspector with no rail — but the lens contract itself is unchanged; see
[`docs/adr/0030-titlebar-navigation-on-demand-inspector.md`](adr/0030-titlebar-navigation-on-demand-inspector.md).
ADR-0032 added a working-set tab strip *inside* the open inspector
(`ProbeToolRail`, MRU-capped, ⊞ opens the ADR-0031 tool grid) for fast
alternation; it renders only while the panel is open and adds no reopen
affordance, so the closed-render discipline stands.

Related Files/Changes and Issues/Pull Requests share a strip slot, with
per-destination context pins and unchanged command IDs. The working-set reducer
owns group identity and replaces a group's remembered subview in place;
recency affects eviction only. The Files subview is persisted independently
of the session-only working set. Agent History reads all database nodes,
including archived rows; reopen restores Suspended without spawning and adopts
the returned row into the live store. See [ADR 0039](adr/0039-desktop-audit-navigation-and-readiness.md).

Routing defaults use `list_routing_options`, a preferences/cached-installation
catalog that does no subprocess or WSL probing. Unverified OpenAI routes stay
disabled pending the live provider menu. Settings owns separate preference,
routing and probe request sequences; account, route and Launch Configuration
mutations refresh both routing sources. Anthropic routes share the live menu's
pure launchability predicate, and configuration-specific errors retain their
remediation. The cheap catalog reads the default WSL distribution only if
startup has already observed it; it never initializes discovery itself.
Failed preferences disable writes, and retries/unmount invalidate stale reads.

### Configuration vs maintenance destinations (issue #1460, ADR-0038)
Two Mesh-lens destinations are split by job and must never merge again:
`properties` (**Project Settings**) owns configuration, `worktrees`
(**Repository**) owns maintenance. Project Settings is the only home for
identity/directory, agent runtime defaults, build and run, and worktree strategy
(use-worktree, base ref, mode, warm pool, worktree directory) — the last group
moved there from Repository. Repository keeps health/recovery, branch and
worktree cleanup, and remote-tracking prune, and imports no `updateMesh*`
wrapper. Both render labelled `ProbeSection` groups, both state that they act
on the project root rather than a focused Agent Node's worktree, and Delete Mesh
lives in a `tone="danger"` section that states impact and recovery before the
shared `ConfirmDialog` repeats the scope. The `probe-<tab>` **ids are
deliberately unchanged** (ADR-0030 keeps them stable for callers and deep links)
even though the source files are named after the destinations; the palette
reaches both through the `Project` tool group and the title bar through one
`More` disclosure. Neither destination takes permanent navigation space.

The Issues and Pull Requests probes are both Mesh-owned GitHub feeds. Their
backend is split by resource under `services::github`: `issues` owns issue
listing, Blocked-by parsing, and trigger-label flags; `prs` owns pull-request
listing, merge strategy, and contributor data; `sync` owns the host token,
HTTP timeouts, and rate-limit classification (a rate-limit body stays an API
error, not a missing repository). `refresh_decision` and
`combine_live_and_cache` live in `sync` as the shared TTL, coalescing, and
live-over-cache rules both probes must use if a snapshot store is added;
live list methods fetch every time today. `services::github` re-exports the
command-facing types, so handlers keep calling `services::github::...`.
Issue-only parsing stays in `issues`; pull-request merge logic stays in
`prs`. Repository lookup in `commands::pr` uses the shared host-path opener.
Unreadable repositories propagate errors to both feeds; only readable repos
without a GitHub origin produce an empty list. WSL ownership trust is an exact
`safe.directory` entry in Windows Git configuration, independent of GitHub
authentication (see [troubleshooting](troubleshooting.md#github-feeds-fail-for-a-wsl-mesh)).

### Probe Panel shell (scroll ownership + narrow width)
The panel and keyed destination wrapper are layout-only, with `min-h-0`,
`min-w-0` and `overflow-hidden`. A destination owns one inner
`flex-1 min-h-0 min-w-0 overflow-y-auto overflow-x-hidden` body; the shared
`ProbeTabBody` supplies this contract. Toolbars and recovery controls are
`shrink-0` siblings. A bounded error excerpt may scroll separately, but must
leave Retry visible. At the dock's 240px minimum, unbounded prose wraps and
unspaced paths/errors use `break-all`. Stating only vertical overflow computes
horizontal auto overflow. Keep Notes' mesh-aware save/restore ownership.

### Frameless Window & Bespoke TitleBar
The window runs with `"decorations": false` (`src-tauri/tauri.conf.json`); `src/components/TitleBar/TitleBar.tsx` is the window chrome (wordmark, ViewModeSwitcher with the Filtered segment, the Filtered view's `GridControls` search bar, the centred "Search or open…" palette field flanked by drag spacers, and the right-hand utility cluster — Usage / Settings / Remote Access pills sharing the `HeaderPillButton` skeleton — plus min/max/close). Traps this recipe has already burned once:
- **Drag regions are per-target.** Tauri's injected script checks `e.target.hasAttribute('data-tauri-drag-region')` — put the attribute on the bar/spacer/wordmark, but *never* on buttons or their SVGs, or the click is eaten and the button starts a drag instead. Double-click maximize on a drag region is built into the same script (`internal_toggle_maximize`).
- **`core:window:default` does NOT cover `allow-minimize`, `allow-close`, `allow-destroy`, `allow-toggle-maximize`, or `allow-start-dragging`** — a frameless window's controls silently no-op without them. Add them explicitly in `src-tauri/capabilities/default.json` (done; keep them if the capability file is regenerated), and remember the grant only exists in binaries built after the change — dev instances running an older binary reject the IPC until rebuilt. `allow-is-focused` is granted there too even though `core:window:default` already covers it: the explicit list IS the record of what the title bar relies on, and the read-only window permissions `core:window:default` grants are exactly the set a future tightening would remove — the caption glyphs' inactive dimming reads `isFocused()`, so don't prune it as redundant (§ADR-0035). `tests/unit/tauri-capabilities.test.ts` is the guard; extend its `required` list when the title bar takes a new window dependency.
- **Application exit must be lifecycle-owned, not a webview-side window IPC.** The exit-confirmation modal's confirmed exit goes through the `exit_application` custom command (`commands::app.rs`), which sets `USER_CLOSE_REQUESTED` and calls `AppHandle::exit(0)` so the `RunEvent::ExitRequested` sweep runs (watchdog expected-exit marker, suspend sweep, agent-process kill). Two reasons it must not call `getCurrentWindow().destroy()` from the webview: window commands are ACL-gated (a binary built before the grant rejects the call — the modal's Exit button would silently no-op), and a bare `destroy` bypasses `CloseRequested` so the `Destroyed` handler needs the `USER_CLOSE_REQUESTED` flag set first to avoid misclassifying a user exit as webview/GPU death (which auto-relaunches). `AppHandle::exit` is fire-and-forget; if the command errors, `exitPromptStore.confirmExit` retracts the expected-exit marking via `cancel_window_close` (same as "Keep Working"), toasts, and resets `exiting`.
- **One writer for window state.** Track `isMaximized` only via the `onResized` listener re-querying `win.isMaximized()`; don't optimistically flip local state on click — a rejected IPC desyncs the glyph.
- **macOS renders traffic lights on the LEFT, not the right.** `TitleBar` branches on `isMac` from `src/lib/platform.ts` (top-level `navigator.platform` read — the project-wide pattern also used by `App`, `Terminal`, `GridNodeHeader`, `paths`, `shortcutCatalog`, `terminalKeyAction`, `TerminalRegistry`). On macOS the right-side square controls are replaced by three circles in `close/minimize/maximize` order, drawn by us and painted from `App.css` tokens (`--color-mac-close` / `-minimize` / `-zoom`) rather than hex literals in the component, so a rest fill and its pressed / inactive variants cannot drift apart. We do **not** reuse Tauri's `titleBarStyle: "Overlay"`: `decorations` is a single cross-platform field (a macOS-only overlay must redeclare the entire `app.windows` array, because config overlays replace arrays rather than merging them), native lights sit near the top of a 28px strip this 45.5px bar does not match, and the change is unverifiable off a Mac (ADR-0036). Platform-conditional tests live in `tests/unit/title-bar.test.tsx` (Windows/Linux default — Vitest's jsdom doesn't match `MAC`) and `tests/unit/title-bar.macos.test.tsx` (forces `isMac: true` via `vi.mock` on `lib/platform`, hoisted before any import resolves — patching `navigator.platform` at runtime is too late because `isMac` is captured at module load).
- **The macOS lights have five states, and two of them are cluster-scoped (ADR-0036).** The glyph reveal belongs to the *strip*, not the button: `group` sits on the `macos-traffic-lights` wrapper, so hovering anywhere over it reveals the × / − / + together as macOS does — putting `group` on a button reveals one symbol at a time, which is the tell that a strip is a web widget. Pressed is a **darker** fill (`active:bg-mac-*-pressed`); there is no hover brightening, because no system light changes fill on hover (the revealed glyph is the cue, and a leftover `filter: brightness()` would tint the glyph with it). An unfocused window greys all three to `--color-mac-traffic-inactive` — the only themed token in the set, since the three fills are identical in both appearances, as on macOS — with `group-hover:bg-mac-*` restoring the colour under the pointer. `useWindowFocused` feeds both this and the caption glyphs' dimming, so the two branches cannot disagree about focus. Geometry is 12px as a **pixel literal** (`h-[12px] w-[12px]`, glyph in a 12-unit `viewBox`): `w-3` is 0.75rem = 9.75px at the 13px root, one size under every other window on the machine. The lights carry no `title` (native ones have none), the state class strings must stay **literal** (a template-built `group-hover:bg-…` emits no Tailwind rule and freezes one state forever), and the green light still calls `toggleMaximize` — native full-screen semantics, the green button's press-and-hold tiling menu, and the Option-modified zoom glyph are AppKit-only and out of scope. See `docs/learning/macos-frameless-traffic-lights.md`.
- **The bar fits the 900px `minWidth` via a degradation ladder, and the root font is 13px.** `html` sets `--font-size-base: 13px`, so every rem-based Tailwind step is 3.25px (`h-9` ≈ 29px, `h-4` ≈ 13px — which is why the caption glyphs are `h-[16px]` literals and not `h-4`) — do spacing arithmetic in those units, not 16px. Tailwind v4 compiles `w-N` to `calc(var(--spacing) * N)` where `--spacing` defaults to `0.25rem`; at the 13px root, `w-80 = 20rem = 260px` (NOT 320px) and `min-w-40 = 10rem = 130px`. The 16px-root arithmetic is a ghost — every spacing value here uses the 13px root. The header is a `grid-cols-[1fr_auto_1fr]` grid (the `auto` centre column + equal `1fr` siblings centre the palette field on the viewport; flanking `flex-1` spacers only centre between unequal clusters). Under flex/grid pressure the search field (explicit wrapper floor `min-w-44`, button floor `min-w-40` = 130px) yields first; the field's design width is **`w-[640px]` — full VS Code Command Palette parity** at ≥1786px viewports where the side clusters can afford it (per PR #1623 review math: at 1440px with switcher labels visible, the left cluster needs ~565px and only allows ~260–310px centre, which matches `w-80`'s 260px). Below 1786px the field uses `w-80` (260px) so the side clusters always fit — typical laptop viewports (1366–1785px) keep the original trigger without losing switcher labels. Measured viewport-width tiers for the viewport media queries: switcher segment AND utility-pill labels share the one tier (return ≥1401px — moved from 1300px in PR #1623 round 4 to avoid a 2px clip on the rightmost switcher segment ("Filtered") at exactly 1300px viewport where labels become visible but the centre's 260px + side clusters' min-content can't coexist; the pills dropped their borders and joined the switcher's ladder in #1609, so the bar never mixes labelled segments with icon pills); the kbd chip ≥1401px — the chip is the FIRST affordance to disappear when narrowing, BEFORE the pill/switcher labels (same tier). User-facing affordances outlast the decorative keyboard hint, per PR #1623 review. Each tier is chosen so `header.scrollWidth ≤ clientWidth` (verified sweep 900→1920 in `fit-sweep.mjs` style). The `GridControls` "Search nodes" bar is the **Filtered view's control**, not a global fixture: it mounts in the left cluster only while `viewMode === 'filtered'` and unmounts entirely otherwise (no idle wrapper or margin — spacing lives on GridControls' own root). The tier widths above are measured in non-Filtered modes; the **Filtered state was separately swept 900→1920 with the bar mounted** (icon-only tier): the left cell gains `w-56 min-w-36` of input, the palette field's bounded centre offset absorbs it, and `scrollWidth ≤ clientWidth` holds at every width. Below ~1400px the side tracks sit at their content minimums, so the field carries a bounded (~≤120px) centre offset; `WindowControlButton` keeps `shrink-0` wrappers and is identified in tests by `data-window-control` (ADR-0035 replaced the old `button.w-11` discriminator, which silently stopped matching when the caption width changed). The Tailwind arbitrary class strings (`max-[1399px]:hidden`, `min-w-40`, `min-[1786px]:w-[640px]`) are **literal strings**, never template literals — Tailwind v4's source scanner only sees static strings, so a `\`max-[${X}px]:hidden\`` constructed at runtime would defeat JIT and the rule would never compile.
- **`--config` overlays REPLACE `app.windows` (arrays don't merge).** `tauri.dev.conf.json` must redeclare every window field from the base config or the dev profile silently reverts it — that's how the dev build lost `decorations: false` and grew a native Windows title bar. `tests/unit/tauri-dev-config.test.ts` pins the overlay to the base; run it after touching either file.
- **Windows 11 Snap Layouts need a native child window, not CSS (ADR-0035).** Windows only offers the flyout to a window whose `WM_NCHITTEST` answers `HTMAXBUTTON`, and the page's WebView2 child HWND answers the hit test first, so the Tauri window's own procedure is never consulted — subclassing it does not work either, and the tell is that your button keeps its CSS `:hover` and its HTML tooltip (meaning the *webview* got the mouse). `src-tauri/src/windowing/` therefore vendors ~150 lines of Win32 that park a never-painting child HWND over the maximise button returning `HTMAXBUTTON`; `src/hooks/useWindowControlOverlay.ts` measures the button (logical px, reported as a right **inset** so the backend can follow a live resize with no round trip) and subscribes to its `titlebar-overlay:*` events. The style set is load-bearing: `WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS`, **no** extended styles (`WS_EX_LAYERED` costs the hit test, `WS_EX_TRANSPARENT` defeats it), invisibility from never painting. Its teardown runs on `WM_NCDESTROY`, never on `WM_CLOSE` — the close request is *advisory* while the exit-confirmation modal can veto it, so hanging teardown there drops the overlay **and** its `WM_SIZE` subclass on a cancelled exit and silently kills Snap Layouts. A minimized window hides the overlay rather than computing a position from an empty client rect. See `docs/learning/windows-frameless-snap-layouts.md` for the failure catalogue.
- **The snap overlay owns the mouse over the maximise button.** Its DOM `:hover`, `:active` and `onClick` do not fire on Windows: hover/press come from the overlay's events, and a click routes through the *same* `toggleMaximize` the DOM handler calls — so `isMaximized` still has ONE writer (the `onResized` re-query) and the overlay adds a caller, never a second writer. Keyboard activation still runs the DOM handler, so both paths must stay wired; don't add a double-fire guard, since a mouse click lands on the overlay and cannot also reach the DOM.
- **Never hardcode the caption-button geometry in Rust.** The overlay is positioned by arithmetic from the measured box, so a constant that drifts from a Tailwind class stops the flyout appearing and breaks nothing else — a failure with no other symptom. Change the button classes and the overlay follows. The cluster must stay flush to the right edge with nothing to the right of Close, because the inset is measured from that edge. `minWidth: 900` also means the flyout appears but zones narrower than 900px cannot accept the window (Microsoft asks for ≤500 epx); that is an accepted trade-off, not a bug.
- **Caption buttons are 46px full-bleed backplates with codicon glyphs**, matching VS Code's window controls: `w-[46px]` so the three form the standard 138px cluster, translucent `bg-caption-hover` / `bg-caption-pressed` fills declared in `App.css` (`--color-caption-*`, re-pointed under `[data-theme="light"]`, with close's red deliberately theme-independent), and the `chrome-*` codicon outlines drawn as filled `0 0 16 16` paths rather than stroke geometry. They carry no `title` attribute (native caption buttons have none), so `aria-label` is the only accessible name, and their glyphs dim while the window is inactive (`useWindowFocused` re-queries `isFocused()` and follows `onFocusChanged`; `core:window:default` already covers it — the same hook now also greys the macOS traffic lights, ADR-0036). Tests identify them by `data-window-control`, **not** a Tailwind class — the old `button.w-11` sentinel silently stopped matching when the width changed, which made the macOS suppression test pass vacuously.


### Keyboard Shortcut Disambiguation (issue #1409, #1568)
The Omnibar claims `CommandOrControl+K` / `CommandOrControl+P` on macOS, and `CommandOrControl+Shift+K` / `CommandOrControl+Shift+P` on Windows/Linux, as Tauri global shortcuts — captured at the OS layer before xterm sees the keydown. macOS `⌘+K` was previously terminal clear — that moved to `⌘+Shift+K` (issue #1409). On Windows/Linux the omnibar owns `Ctrl+Shift+K`, so terminal clear remapped to `Ctrl+Shift+L` to keep both gestures (issue #1568) — follow the rule: when adding a global shortcut that could collide with an xterm chord, **remap the terminal side or augment the modifier rather than stealing the user's keystroke**. The platform-canonical modifier split (meta on Mac, ctrl elsewhere) lives in `terminalKeyAction.ts` (`resolveKeyAction` / `resolveZoomKeyAction`); the display labels live in `shortcutCatalog.ts`; the App.tsx `shortcuts` array is the binding source of truth. The open/close state lives in `uiStore` (`omnibarOpen` / `toggleOmnibar`, map #1371 Decision #2).

### WSL Path Mapping
Linux paths from WSL agents must map to Windows UNC paths (`\\wsl$\...`) before backend file operations. Use `env::to_host_path` in `src-tauri/src/env/host_path.rs` (the `HostPath` sub-module). The CLAUDE.md hard rule is **structurally** enforced: `HostPath` is the *only* module in the tree that builds `\\wsl$\` or `/mnt/` strings; no other module should. Never pass Linux paths to Windows-side APIs.

Harness installation and mesh filesystem are independent. `EnvType::Windows` retains its legacy host-native meaning on Linux; `WindowsInterop` explicitly selects a Windows process from a Linux WSL host. Startup executable observations filter automatic menu profiles without deleting saved identities, and the spawn menu emits one installation per harness. On Windows, WSL profiles and their saved Launch Configurations are shown only when the backing harness lacks Windows support; saved identities remain available for existing sessions. Windows npm shim directories are excluded from Linux-native discovery. Detected `HarnessProfile.runtime` overrides are persisted in `AgentNode.env` at creation/provider change, before acquiring the database writer. `node_working_path` uses that durable runtime to derive the process path while retaining host/raw filesystem paths; it must not load preferences under callers' database connections. Discovery probes the default WSL distribution, records its identity in profiles, and caches `wslpath` drive mounts. Guest launches use the cached distribution explicitly with `wsl.exe --exec` and a login-shell PATH; `--` alone reintroduces default-shell interpolation and corrupts literal prompts. Guest configuration/session homes come from the guest login, not the Windows username.

Cross-runtime linked worktrees retain a host administrative backpointer and a relative forward `.git` link. They are locked against guest pruning because older Git treats foreign absolute backpointers as missing, and does not safely support relative backpointers without newer repository extensions. Buildmesh owns removal and already explicitly prunes locked entries. Prepare this metadata on the blocking pool. Windows process sandboxing cannot contain WSL agents; reject that combination. Shell hooks for a Windows host use Windows curl from WSL to reach host loopback even under NAT; Linux-hosted Windows callbacks explicitly invoke curl in the owning WSL distribution, avoiding collisions with Windows Buildmesh on localhost. Native Linux callbacks retain native curl. Grok uses platform-specific command hooks with curl because its native HTTP-hook SSRF guard rejects the local plain-HTTP callback; those commands still require loopback reachability, including mirrored WSL networking where applicable.


### Agent Spawning on Windows
Each adapter's `spawn_recipe` (`src-tauri/src/agent/provider/adapters/<id>.rs`) declares a `WindowsShell`: `PowerShell` where ANSI output must propagate (Codex, the plain terminal), `Cmd` for npm `.cmd` shims (e.g. OpenCode, MiniMax Code, DeepSeek Harness, Cline on Windows), or `Direct` for native binaries (e.g. Claude Code, Kimi, Antigravity, Grok). `spawn_environment::wrap` consumes it; macOS/Linux always spawn `Direct`. The adapter is the source of truth — read it rather than trusting this list.

### Database Pattern
Use `_inner` helper functions that accept `&Connection` so compound operations keep one connection and writer transactions never re-lock. Public read functions check out `read_conn()` from the eight-connection read-only pool; public mutations lock the dedicated `write_conn()`. SQLite WAL lets those readers run concurrently with the serialized writer. Async request paths use `try_read_conn()` so pool contention has a bounded wait and an error result. See `src-tauri/src/db/mod.rs`.

### Command Threading (blocking work must not touch the async worker pool)
A `#[command]` on an `async fn` **and** `#[command(async)]` on a sync `fn` both run on Tauri's bounded tokio worker pool (≈ CPU cores). Only a plain sync `#[command] fn` runs off it. So a command that does a **blocking network call** (`reqwest::blocking`, `git fetch`/`git pull` shell-out), a **SQLite transaction** (`db::*` / `db::write_conn`), a **disk read/write** (`std::fs::*`, `preferences::load`/`save`), or a slow libgit2 walk on the async runtime **parks a worker for the whole duration**; enough of them stuck at once starves the pool and every other async command (agent keystrokes, WebSocket streaming, probes) stops being polled while the UI stays alive — the class of bug behind the overnight-freeze (issue #762 / #1380: see the `run_blocking` wrappers in `commands/pr.rs`, `commands/github.rs`, `commands/agent_node.rs`, `commands/preferences.rs`). Convention: give each such command a **plain-sync core (`*_blocking`)** and a thin `#[command] async fn` wrapper that offloads it via `crate::commands::run_blocking(label, || core(..))` (which threads it through `tauri::async_runtime::spawn_blocking`). Fast in-memory lookups may stay as a plain sync `#[command] fn` (the circuit CRUD commands do this). The mobile HTTP routes (`http/routes/*`) are **not** a separate pool — `http/mod.rs` spawns each connection on the same `tauri::async_runtime`, so a route that calls a `*_blocking` core directly still parks a worker; routes are `async fn`, so they must **`.await` the async command wrapper** (e.g. `get_repo_issues(id).await`) or `run_blocking` themselves, letting it offload. Gated by `tests/unit/async-command-blocking.test.ts` (issue #1380); per-line opt-out: `// allow-blocking-on-async: <reason>`. Only reach for a `*_blocking` core from a genuinely synchronous context (e.g. `check_gh_auth_cached`, itself run inside `run_blocking`). Also give any blocking network client a finite `.timeout(..)` (`GitHubClient` uses `build_http_client`) so a half-open connection can't hang forever.

### Injectable Caches (avoid process-global statics)

The frontend Git-query caches share one `GIT_CHANGED` bus in
`src/lib/pathInvalidatedCache.ts`. A dispatch invalidates each client/key once
before notifying its consumers, so their refreshes share the same request.
Events during a slow request defer one follow-up refresh rather than repeatedly
superseding it; the running request can publish while edits continue.
Only the currently registered pending promise may commit a value/error or clear
request ownership; superseded callers adopt the current request/value. Trailing
refreshes retain subscription objects and unsubscribe removes them, cancelling
the timer when no consumers remain. The native file-watcher coalescer uses a
single-slot non-blocking wake channel: signals contain no file data and one wake
is enough to request the latest state. See the
[performance review](development/performance-audit-2026-10.md) for evidence and
remaining retention/coverage work.

Process-global `static OnceCell<Mutex<_>>` caches with test-only `#[cfg(test)]` forks (or per-test `GH_AUTH_CACHE_TEST_LOCK`) are flaky — `cargo test -- --test-threads=8` still racy via snapshot-delta tricks and they hide production synchronization bugs (see #1482 preferences, #1483 gh-auth). Prefer an injectable struct (`services::gh_auth_cache::GhAuthCache` — single `Arc<Inner>` with `slot: Mutex<(Instant,bool)>` + `misses: AtomicU64` + injectable `now`/`auth` closures, `Mutex` held across `auth_fn` to coalesce concurrent misses to one HTTPS call). Wire one instance via `tauri::Builder::manage(GhAuthCache::new())` and take `tauri::State<GhAuthCache>` in the `#[command] async fn`; the `*_blocking` core takes `&GhAuthCache` and calls `cache.check()` directly (runs on `spawn_blocking`, so blocking there is the intended coalescing). Tests use `GhAuthCache::for_test_with_auth()` per `#[test]` (isolated counter, stubbed network, controllable `now` via `for_test_with_clock_and_auth` or `expire_for_test()`). See `src-tauri/src/services/gh_auth_cache.rs` and `src-tauri/src/lib.rs:manage`.

### Pattern Guards (lint-style unit tests)
The repo runs a small fleet of "pattern guard" unit tests that walk source files and fail on text patterns known to ship a silent regression. They live under `tests/unit/` and run in the standard `npm test` / `scripts\check.ps1 unit` loop:

- `tests/unit/ipc-contract.test.ts` (issue #163) — every `invoke('name', …)` literal in `src/` must be registered in `tauri::generate_handler![…]` in `src-tauri/src/lib.rs`. Companion: each `#[command]` MUST be added to that handler list too (the symmetric trap).
- `tests/unit/webapi-on-this.test.ts` (issue #156) — forbids `this.x = <WebAPI>` (e.g. `this.scheduler = requestAnimationFrame`). In Chromium/WebView2 the WebIDL receiver binding throws "Illegal invocation" when the API is invoked through an object property. Fix: wrap the API in an arrow function (`this.scheduler = (cb) => requestAnimationFrame(cb)`) or `.bind(window)`. Per-line opt-out: `// allow-webapi-on-this: <reason>`, mirroring the `// allow-dispose` / `// allow-wsl-path` convention enforced by `.claude/hooks/guard-antipatterns.mjs`. See memory `buildmesh-webapi-receiver-binding` for the full receiver-binding story.
- `tests/unit/async-command-blocking.test.ts` (issue #1380) — an async `#[command]` in `src-tauri/src/commands/` must not call `db::*`, `std::fs::*`, or `preferences::load`/`save` except inside `run_blocking` / `spawn_blocking`. Per-line opt-out: `// allow-blocking-on-async: <reason>`.
- `tests/unit/radii-audit.test.ts` (issue #733) — no bare `rounded` (or bare directional `rounded-r` / `-l` / `-t` / `-b` / …) may survive anywhere in `src/components/` unless it is the one intentional chip pinned in `ALLOWED_BARE_ROUNDED`. Use a size-suffixed variant (`rounded-md` / `-sm` / `-lg` / `-full` / …) or pin the line with an `allow-bare-rounded` marker. The radius conventions themselves live in `DESIGN.md`; this guard only enforces them.
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
- **Per-harness capability *values* (ADR-0037):** ts-rs generates the `HarnessCapabilities` *type*, not the per-adapter table. The total catalog is `builtin_harness_catalog()` in `src-tauri/src/agent/harness_catalog.rs` — every `Provider` variant, `adapter().capabilities()`, plus the Inspector/docs label, independent of detection. `cargo test` writes `src/types/generated/HarnessCapabilitiesTable.ts` (and `.json` for the docs gates). The Circuits Inspector and README/docs coverage consume that artifact. A hand-written TypeScript mirror of those values is a defect; `list_providers` is not a substitute because it only returns detected/configured rows. The profile id `claude` is an alias of the `anthropic` adapter (`HARNESS_PROFILE_ALIASES`), not a silent default.
- **Probe prompt preview contract:** `agent::spawn::intent` exports `src/types/generated/ProbeSpawnPromptExamples.json` during `cargo test`, using the production issue/PR renderers and template constants. The settings tests compare rendered previews, token buttons, and sample values against this fixture. Regenerate it when changing spawn templates, substitutions, or the shared review policy; CI's generated-artifact drift gate covers it.
- **Still hand-maintained (migrate later):** `src/lib/status.ts`'s `SessionStatus` (a UI-config copy), and the `Diff*`/`FileNode`/`OpenPr`/`GitBranchStatus` types in `tauri.ts`/`api.ts`. These are not yet generated.

## Anti-Patterns (DO NOT do)
- ❌ Call `dispose()` on an xterm.js Terminal — causes permanent terminal blanking
- ❌ Pass Linux paths (e.g. `/home/user/`) to non-WSL APIs — causes "file not found"
- ❌ Spawn cwrap directly without `cmd.exe /c` on Windows — ConPTY breaks
- ❌ Spawn a provider CLI to fetch a Usage Meter when the CLI is wrapping an HTTP endpoint we can call ourselves. `get_provider_meters` waits for every provider, so a multi-second CLI boot stalls the whole Usage Probe (#1324 spawned `agy --print /usage` ≈6s; the same payload is `POST daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary` in ~250ms, User-Agent gated). Token discovery lives in `usage::adapters::agy`: prefer `<agy_dir>/antigravity-oauth-token` (Windows: `%USERPROFILE%\.gemini\antigravity-cli\antigravity-oauth-token`, honouring `GEMINI_HOME` / `ANTIGRAVITY_HOME`), then Windows Credential Manager `gemini:antigravity`. Corrupt CLI-file JSON surfaces as Shape (no keyring fallback); missing/empty falls through. On HTTP 401/403, try the next source before logged-out. `fetchAvailableModels` is five-hour-only fallback.
- ❌ Lock the DB mutex in nested calls — causes deadlocks
- ❌ Do blocking network / git-shell-out / slow-libgit2 / SQLite (`db::*`) / `std::fs::*` / `preferences::load`/`save` work directly on an `async fn` (or `#[command(async)]`) command — it parks a tokio worker and, at scale, starves the pool (UI stays alive, keystrokes + WebSocket streaming + probes hang). Use the `*_blocking` sync-core + `run_blocking` wrapper; see *Command Threading* (issue #1380).
- ❌ Give a Probe tab root its own `overflow-y-auto` — `ProbePanel` already wraps it in one, so you get two stacked scroll owners and an unpredictable scroll surface (#1468). Root is layout-only; one inner body scrolls. And don't `truncate` unbounded text (errors, trigger identities, node ids) at the dock's 240px minimum — it clips exactly the tail that carries the diagnosis. See *Probe Panel shell*.
- ❌ Compare a zoneless SQLite timestamp (`"2026-08-22 10:05:00"`, what `CURRENT_TIMESTAMP` writes) against `Date.now()` via a bare `Date.parse`. V8 accepts the shape and reads it as **local** time, so the value is silently wrong by the host's UTC offset. The skew cancels when you subtract two ledger timestamps — which is why `stepDurationMs` hid it for months — but not against an absolute clock. Parse through `ledgerTimestampMs` in `circuitGraphModel.ts`, which forces `Z` on any zoneless timestamp.
- ❌ Ship `<a target="_blank">` for an external URL — Tauri 2's WebView is not a browser, the click is silently dropped without the `core:webview:allow-create-webview-window` capability (which we don't grant). Keep the `href`/`target`/`rel` and route the `onClick` through `openUrl()` from `@tauri-apps/plugin-opener` (e.g. `src/components/SessionView/GridNodeHeader.tsx:145`). The right-click "Open in browser" path still works, which makes the bug look like a click-handler issue — it isn't.
- ❌ Read a request body with bare `BufStream::read_exact` (or `read_line` for the head) without a `tokio::time::timeout` wrapper. A client that advertises a Content-Length and dribbles bytes pins a tokio worker for the entire upload window — a slowloris that hits every POST body and every WebSocket header read. The single seam is `crate::http::request::read_body_with_cap` (invoked by `http::server::handle_connection` from the route's `BodyPolicy`); `REQUEST_HEAD_TIMEOUT` wraps the head read in that same function. Route handlers take `ParsedRequest` (body already read) and must not reimplement the read.
- ❌ Store a Web API as `this.x = requestAnimationFrame` (or `setTimeout` / `fetch` / `MutationObserver` / etc.). Chromium WebIDL bindings enforce the receiver — calling the API through an object property throws `TypeError: Illegal invocation` and the throw lands inside a Tauri listener that swallows it, so the symptom looks like "events not arriving" rather than a crash. Always wrap: `this.scheduler = (cb) => requestAnimationFrame(cb)` (the form `TerminalWriter` uses, `src/components/Terminal/TerminalWriter.ts:60`). Pinned by `tests/unit/webapi-on-this.test.ts`; opt-out `// allow-webapi-on-this: <reason>` on the violation line. Memory: `buildmesh-webapi-receiver-binding`.
- ❌ Nest a `position:fixed` overlay (context menu, dialog) inside an ancestor that has `filter` (`hover:brightness-*`), `transform` (dnd-kit sortable), `opacity` other than 1, or `backdrop-filter`. Those properties create a containing block, so `top`/`left` from `clientX`/`clientY` are no longer viewport coordinates — the overlay jumps, then auto-focus scrolls the nearest `overflow` ancestor. Portal to `document.body`. The shared `Modal` primitive already does this (issue #1292), so `<Modal>` and `<ConfirmDialog>` (its thin wrapper) are safe to mount anywhere; only click-anchored menus (`NodeItem` / `MeshItem` sidebar context menus) still need to portal at the call site. Don't put `preventScroll` on the shared `useAriaMenu` hook: `ProviderDropdown` is itself `overflow-y-auto` and needs default focus-scroll so arrow keys can reach items below the fold.

## Credentials (Windows Credential Manager)

The Buildmesh-managed OAuth secrets live in Windows Credential Manager under `CRED_TYPE_GENERIC` (the catch-all "store arbitrary bytes" type — domain credentials are a separate `CRED_TYPE_DOMAIN_*` family and we never use them). FFI is hand-rolled over `advapi32!CredReadW` / `CredWriteW` / `CredDeleteW` rather than `windows-sys`, matching the project's "minimal-FFI for the two-or-three functions we actually call" convention (also used by `sandbox::restricted_token`).

- **Surface** (`src-tauri/src/services/windows_cred.rs`):
  - `read(target: &str) -> Result<Vec<u8>, UsageError>` — missing credential collapses to `NoCredential(target)`; empty blob bytes round-trip as `Vec::new()` so a higher-level parser can decide what "empty" means.
  - `write(target: &str, blob: &[u8]) -> Result<(), UsageError>` — upsert via `CredWriteW` with `CRED_PERSIST_LOCAL_MACHINE` (persists across reboots, local-user-scoped; never `CRED_PERSIST_SESSION` — OAuth tokens need to survive logoff — and never `CRED_PERSIST_ENTERPRISE`, which requires domain policy we don't ship). `UserName` is set to the same string as the target so Credential Manager's detail view is manageable.
  - `delete(target: &str) -> Result<(), UsageError>` — **idempotent**: a `FALSE` return followed by `GetLastError() == ERROR_NOT_FOUND (1168)` collapses to `Ok(())` so the Settings "Sign out" affordance never errors on a no-op. Any other Windows failure surfaces as `Shape(target, GetLastError)`.
  - `cfg(windows)` only. Non-Windows callers see `NoCredential(...)` from their `cfg`-gated helpers instead.

- **Known targets** (extend-only — never delete from this list without a migration ticket):
  - `gemini:antigravity` — written by older Antigravity CLIs, read-only here. Current CLI (1.2+) refreshes `<agy_dir>/antigravity-oauth-token` instead and leaves this target stale; the Usage Meter prefers the file, then this keyring, and retries the alternate source on HTTP 401/403.
  - `opencode:console` — written by Buildmesh for the OpenCode Go OAuth dance (issue #956). Persisted blob is JSON `{ access_token, workspace_id, refresh_token, expires_at, server_id }` (RFC-3339 string for `expires_at`, mirroring the original #957 fixture so the live probe still parses; the `server_id` field is the SolidStart deployment id captured into the `X-Server-Id` header).

- **Operator commands** for diagnosing drift:
  - `cmdkey /list | findstr antigravity` — confirm the legacy `gemini:antigravity` keyring target is present (metadata only; does not dump the blob).
  - `Get-Content $env:USERPROFILE\.gemini\antigravity-cli\antigravity-oauth-token` (or `$env:GEMINI_HOME\antigravity-cli\antigravity-oauth-token`) — inspect the live CLI oauth file; redact before pasting. Compare `token.expiry` against the keyring blob when the Usage Probe drops Antigravity.
  - `cmdkey /list:opencode:console` — read the current blob's user/credential metadata without dumping bytes.
  - `cmdkey /list:buildmesh-test-*` — find any leftover test credentials from a failing test that didn't clean up. Each unit test uses a uuid-suffixed target name so collisions are vanishingly rare.

- **Pitfalls** — each caught by an iteration of real bugs:
  1. **`GetLastError` is mandatory after a `CredDeleteW` FALSE return.** Microsoft conflates "didn't exist" with "real failure" by returning FALSE for both, so a TRUE-only check would shadow the idempotent revoke the Settings UI relies on.
  2. **`from_raw_parts` requires non-null even for length 0.** Guard with `if cred.credential_blob.is_null() || cred.credential_blob_size == 0` to avoid UB on a freshly-written credential whose blob pointer hasn't been allocated.
  3. **`CRED_PERSIST_SESSION` is wrong for OAuth tokens.** A credential with this flag is gone after logoff — fine for session-only secrets (the kind cwrap adapters carry), wrong for a long-lived refresh token.
  4. **The Rust blob format is implicit.** Buildmesh's parser (`services::opencode_oauth::parse_opencode_console_full_credential`) currently shapes a 5-field blob: `access_token` + `workspace_id` + `refresh_token` + `expires_at` + `server_id`. The first two are required for the live probe (see `parse_opencode_console_credential`); the last three ride along for `try_refresh` and the `X-Server-Id` header fallback. Any future extension that adds a sixth field must update both the writer (`services::opencode_oauth::persist_token_response`) AND the parsers atomically, or the live probe will silently drop the field (serde defaults — `skip_serializing_if = "Option::is_none"` on the writer + `#[serde(default)]` on the reader means both sides tolerate missing keys but never notice renamed ones). `parse_full_credential_round_trips_all_five_fields` pins the contract; CI's `git diff --exit-code src/types/generated/` is the wire-shape gate, but not the Rust-side wire-shape gate — keep both directories in sync by hand until both ts-rs exports and Rust struct shape are auto-derived.

## State recovery: snapshot, export, integrity check, restore (issue #1537, [ADR 0040](adr/0040-state-recovery-snapshot-export-restore.md))

`services::state_recovery` owns the profile's durable state lifecycle. It is the **only** module allowed to capture, replace, or validate `buildmesh.db` as a whole; domain modules (`db::mesh`, `db::agent_node`, …) read and write rows and must never touch the file as a file.

- **What "durable state" means here** — three things, not two: `buildmesh.db` (SQLite), `preferences.json` beside it, and `tls/` holding the LAN root CA **private key**. The service owns the first two end to end and deliberately does **not** own the third. Windows Credential Manager holds provider OAuth blobs outside any file and is not exportable; that gap is tracked in #830.
- **Capture uses `VACUUM INTO`, never a file copy.** A WAL-mode database's committed content may still live only in the `-wal` sidecar. The capture opens a **private** connection rather than `db::read_conn()` — `VACUUM INTO` writes its destination file, and the project rule is that filesystem I/O must never run while a pooled or shared DB handle is held (issue #1228). A private connection also cannot deadlock against the writer mutex it is capturing.
- **Start-up hooks live in `run_profile_startup`, not in `db::`.** `apply_pending_restore` runs first (before `db::init`, so no connection, reader pool, circuit worker, or PTY exists), then `snapshot_before_migration`. Both open their own read-only connections to read `schema_version`, which keeps the layering `commands → services → db` — nothing in `db/` grows a dependency on `services::`, and `db/seam_tests.rs`'s `ALLOWED_PUB_CRATE_FNS` allowlist needs no entry. Both are **non-fatal**: a recovery failure must not block launch, because the user needs the UI to reach Restore. Failures are logged and surfaced as a `RecoveryNotice`.
- **Exports are a versioned `.bmsnap` container, not a folder zip.** `magic | u32 header_len | JSON header | payload blob`, with a SHA-256 per section. A zip of the app-data directory would sweep in `tls/ca.key.der` and the cleartext `remote_access_token`; the container gives a format-version gate, per-section integrity, and an explicit section list so "this export contains no TLS keys" is a checkable fact.
- **Redaction runs against the copy, never the original.** `build_bundle` captures to a staging file and deletes the credential rows/fields from *that*, so a bug in the redaction path cannot reach live state. `preferences.json` redaction deliberately does **not** round-trip through the `AppPreferences` struct — an export must survive a file written by a newer build, and going through today's struct would silently drop every field it does not know about.
- **`tls/ca.key.der` is never exported and has no "include secrets" toggle.** Anyone holding that key can impersonate the HTTPS identity every paired device trusts. Terminal transcripts are absent because durable state never held any (scrollback is xterm.js; agent transcripts live in harness session directories outside the profile).
- **Restore is staged, never immediate.** `stage_restore` verifies → snapshots the current state for rollback → extracts the payload and fsyncs it → *then* writes the marker. The marker-last ordering is what makes a crash mid-stage inert. `apply_pending_restore` deletes the stale `buildmesh.db-wal`/`-shm` sidecars before moving the restored file in: without that, SQLite replays the *old* database's WAL frames onto the restored file and corrupts it. It also re-runs both `quick_check` and `integrity_check` on the staged payload, because the bytes may have changed since staging.
- **Never silently reset.** A failed pre-migration check still writes a snapshot; when `VACUUM INTO` cannot run on a damaged file it falls back to a raw byte copy (labelled `*-raw`, with a `RecoveryNotice` naming the path) because losing the only copy is the unacceptable outcome. Integrity checks only ever *report* — nothing in this module rewrites a database to make a check pass.
- **Retention is `SNAPSHOT_RETENTION = 3`**, ordered by the timestamp slug in the filename with mtime as a tiebreak. Manual snapshots share the cap.
- **Wire types are ts-rs derived** (`StateRecoveryInfo`, `StateSnapshot`, `StateIntegrityReport`, `StateExportResult`, `StateRestorePlan`, `RecoveryNotice`) — never hand-declared in TS (issue #359). Frontend wrappers live in the `src/lib/tauri/stateRecovery.ts` facet and route through the `_invoke` chokepoint (ADR-0010). Save/open dialogs resolve **in Rust** (matching `commands::mesh::pick_mesh_folder`), because the app-data directory is not readable from the frontend and the capability file grants no filesystem access. A cancelled dialog returns `null`, which is a no-op and not an error.
- **Tests:** `src-tauri/src/services/state_recovery/tests.rs` (parallel-safe; private temp dirs, never the process-global `DB`) and `tests/unit/data-recovery-settings.test.tsx`. The rejection cases all route through `assert_rejected_without_side_effects`, which asserts the live database and preferences are byte-identical afterwards and nothing was staged.

## Coordinator Read & Drive API

Buildmesh exposes an **HTTP surface** for an external **Coordinator** (the user's remotely-hosted Hermes Agent first; a future in-app superagent second) to scan every Agent Node across every Mesh, drill into any one (the **read** half, below) and **drive** a chosen node (the write half — see *Coordinator Drive*). Plain JSON over the existing embedded HTTP server, **off by default**, behind separate capability-scoped tokens distinct from the mobile root token, bound to loopback + LAN (the user owns the remote tunnel — Tailscale / Cloudflare / WireGuard). Hermes is one instance of a Coordinator, not the category.

- **Architecture & rationale:** [`docs/adr/0008-coordinator-control-api.md`](adr/0008-coordinator-control-api.md). **Domain language:** *Coordinator*, *Node Digest* in [`CONTEXT.md`](../CONTEXT.md).
- **User guide (how to enable + consume):** [`docs/development/coordinator-read-api.md`](development/coordinator-read-api.md). **Spec:** issue #312.
- **Two endpoints (both authenticated with the read-scoped bearer token):**
  - `GET /nodes` — array of layered Node Digests. Spine is always present (lifecycle `status`, `needs_feedback` = `awaiting_input`, `waiting_since`, `last_activity`); the transcript-derived rich layer is present for providers with a wired reader (Anthropic/Claude-compatible profiles, Codex, Cursor, AGY, Grok, Command Code, Muse) or explicitly flagged `unavailable` (degrade-and-flag, never a silent omission). Muse nodes may also include `observed_session_telemetry` when MSP token/context events have been ingested; the field is omitted when there are no observations and is never remaining account quota. Cheap to poll.
  - `GET /nodes/{id}/log?tail=N` — on-demand raw recent turns (assistant text + tool calls) for one node. Content is **raw, not pre-summarised** — the Coordinator is itself an LLM. An unknown node id is a 404; every other degrade path is a 200 carrying a structured `unavailable` envelope.
- **Module layout:** `src-tauri/src/coordinator/` (`node_digest.rs` is the pure digest builder; `enrichment.rs` owns provider capability, working-path resolution, secret scrubbing and Muse observed-session telemetry). `src-tauri/src/services/transcript_reader/mod.rs` preserves the public read signatures and shapes unavailable results once. The `TranscriptReader` seam in `transcript_reader/adapter.rs` dispatches to one module per wired format in `transcript_reader/readers/`; each owns location, parsing, digest and assistant-report reads, including OpenCode's SQLite store and Cline's whole-document history. Shared streaming and byte-window I/O lives in `transcript_reader/file.rs`. The `adapters` module alias preserves existing lifecycle and session-discovery imports. `src-tauri/src/http/routes/coordinator.rs` is a thin transport skin; `src-tauri/src/http/router.rs` enforces the off-by-default + read-token gate in the dispatcher.
- **Contract test guard:** reader-owned tests use real disk fixtures under `src-tauri/tests/fixtures/transcripts/<reader>/` and temporary native stores to cover tail selection, latest assistant text, malformed records, empty sessions, unavailable files and shape drift. `TranscriptFormat::for_harness` explicitly lists Claude-compatible profiles and returns `None` for unwired ids; transcript dispatch never borrows Claude content. A coordinator regression preserves the Node Digest spine with enrichment flagged unavailable. This is the read-side form of the project's serde-default-fragility lesson.
- **Coordinator Drive (PRD #313, ADR-0008 §5–6; D1 #319 + D2 #320).** The write half: `POST /nodes/{id}/prompt` writes a prompt into a live node's PTY through #178's `AgentDriver` (`send_prompt` → `verify_delivery`) — the PTY's stdin *is* the input box, nothing is screen-scraped, and **any live node** is drivable (Claude Code queues stdin for a busy agent). Requires the **drive scope** (distinct from read, off by default, under the master kill-switch); the read token can never drive. The response carries an **honest verdict** — `Delivered` on a confirmed `awaiting → cleared` transition, else `Unverified` (queued-but-unconfirmed) — never success without confirmation. All the drive logic lives in `src-tauri/src/coordinator/drive.rs` behind two seams (`DriveTarget` = PTY/DB/events, `IdempotencyStore` = the ledger) so it is unit-testable without a real PTY or DB; `http/routes/coordinator.rs::prompt` is a thin skin. Do **not** grow a parallel write path — the scheduler (#178) reuses the same `AgentDriver`.
- **Idempotency (D2 #320, hardened #750).** Each drive carries a mandatory caller-supplied `idempotency_key`; the `coordinator_drive_prompts` ledger (`PRIMARY KEY (node_id, idempotency_key)`, `db/mod.rs`, schema v32) records the verdict once and a duplicate key **replays** it instead of re-sending, so a Coordinator's retry over a flaky network never lands a prompt twice (#178's cardinal rule). v32 (issue #750) reshaped the row protocol from `lookup → send → record` (racy: two concurrent same-key requests could both send) to atomic **claim-before-send** — the claim transaction inserts a `pending` row, the winner drives, the loser sees `InProgress` and briefly waits for finalize (or surfaces `409 + Retry-After: 1`). A `pending` row older than `PENDING_CLAIM_TIMEOUT_SECS` (30 s) is reclaimed by the next claim attempt, so a crashed-mid-send row can't lock out the key. The row also carries a SHA-256 `prompt_hash` (Stripe-style item 2 hardening) — same key + *different* prompt is `409 key_payload_mismatch` rather than a silent 200-replay-of-different-prompt. Recording happens only after a successful send: `NotLive` / `WriteFailed` calls `release_claim` (so a retry re-attempts); `Delivered` / `Unverified` calls `finalize` (so re-sending is the double-delivery #178 forbids). Lookup **fails safe, not open** — an unreadable ledger returns `503`, never a silent re-send (a read error must never be mistaken for "key never seen"). GC: a dedicated background worker (`services::coordinator_ledger_maintenance::start_worker`) prunes rows older than `LEDGER_RETENTION_DAYS` (7 days) on a 30-minute cadence so the table's size is proportional to "unique drives per week" rather than "unique drives ever" (item 3). The pure `drive_node_idempotent(store, driver, …)` orchestrator encodes claim → send → finalize / release_claim and is tested against fakes (headline: same key twice = exactly one PTY write; same key + different prompt = `KeyPayloadMismatch`; concurrent same-key = exactly one delivery, the loser waits then sees Replay).
- **Tunnels are the user's job.** Buildmesh never opens an internet port — the threat model for the coordinator surface is "coordinating agent on a machine I control, reached over my own tunnel", not "autonomous agent on a public VPS reachable from the open internet". Reaching the read surface from outside the LAN is a deliberate user choice, not a Buildmesh feature.
- **Auth is two-tier and header-only (#500, ADR-0015).** Every request resolves to a [Role](../CONTEXT.md) — **Admin** (root token, the mobile `/api/*` surface) or **Coordinator** (read/drive tokens, `/nodes*`) — as **disjoint surfaces**: a token works only on its own surface (wrong-surface valid token → 403, no creds → 401). Role resolution lives in `src-tauri/src/http/auth.rs` (`authorize`); the dispatcher (`http/router.rs`) calls `auth::authorize(.., scope)` per route, and `/admin/*` is reserved Admin-only. Credentials travel only in `Authorization: Bearer` or the `bm_session` cookie — never `?token=`. The mobile shell/assets are public; the client logs in via `POST /api/session` (sets the cookie) and mints a single-use `?ticket=` per WebSocket via `POST /api/ws-ticket` (`src-tauri/src/http/ws_ticket.rs`).
- **Device Sessions are the per-browser Admin credential.** Desktop IPC `create_pairing_ticket` mints a hashed, single-use five-minute invitation (`http/pairing.rs`). QR URLs put it in `#pair=`, outside the initial request. `POST /api/pair` consumes it and creates a hashed `device_sessions` row. `POST /api/session` refreshes existing devices only, including one-time migration of old localStorage device credentials. Neither endpoint accepts root-token pairing or returns credentials in JSON. Both set a 400-day HttpOnly, SameSite=Strict cookie (Secure over TLS). Mobile boot refreshes automatically. Site-data removal, address changes, cookie expiry/eviction, and desktop revocation can require re-pairing. Authentication never depends on client IP. Authorized Devices and remote admin revocation delete the device row and broadcast to close its WebSockets; WebSockets use per-target single-use tickets.

The native Android client in `android/` consumes the same remote HTTP routes and
per-device session protocol. Compose owns management screens; a bundled, isolated
xterm WebView renders only terminal bytes while Kotlin owns transport. Pairing
authenticates the desktop CA against the QR fingerprint before exchanging an
invitation. Android Keystore protects the persisted session. Foreground refreshes
coalesce with owner/revision fences, and forgetting cancels pending actions. See
[native Android client](development/android.md) for build and boundary details.

## LAN/VPN Exposure & Self-Signed TLS

The embedded server binds **loopback only by default** (#496). An off-by-default
**LAN / VPN Exposure** toggle (App Settings → `set_lan_exposure_enabled`, stored in
`app_settings.lan_exposure_enabled`) exposes it on the machine's interfaces; issue
#501, [`docs/adr/0017-opt-in-lan-exposure-and-self-signed-tls.md`](adr/0017-opt-in-lan-exposure-and-self-signed-tls.md).

- **Loopback stays plain HTTP; only non-loopback interface IPs get TLS** (`http::bind_specs`). This is deliberate: the attention webhook posts plain `http://localhost/api/attention/...`, so forcing TLS on loopback would silently break every agent's "awaiting input" signal. Do **not** "simplify" this to a single `0.0.0.0` TLS bind.
- **TLS identities** use a stable root and a leaf covering localhost, loopback, and interface IPs. `http/tls/identity.rs` serializes writers, validates key/certificate matches and the leaf chain, stages and syncs a complete private generation, then atomically publishes `tls/current`. Readers resolve the pointer once. Legacy files migrate without changing a valid root; address changes renew only the leaf. Explicit reset publishes a fresh root. Prior complete generations remain available; incomplete staging never becomes active. Directories and private keys are owner-only, including the temporary iOS signing directory.
- **The toggle rebinds live** without an app restart. A serialized binding operation owns the setting snapshot and listener replacement. Each listener owns its TLS/HTTP/WebSocket tasks and aborts/drains them on shutdown; loopback listeners have separate ownership. Each listener admits at most 64 HTTP/handshake connections and 32 WebSockets, with a five-second TLS deadline. WebSocket output futures remain inside their connection. Disabling LAN also invalidates unused pairing invitations.
- **HTTP module layout (issue #1658):** `http/server.rs` (bind, TLS, accept, head parse, Host guard, body read, WS upgrade), `http/router.rs` (one `ROUTES` table + `dispatch(ParsedRequest)`), `http/state.rs` (app handle, snapshots, ports, listeners, interface cache). `http/mod.rs` re-exports and exposes `start_http_server` / `reapply_binding`.
- **`MaybeTls`** (`http::stream`) is the single concrete stream type (`Plain(TcpStream)` | `Tls(Box<TlsStream<TcpStream>>)`); WSS rides the same enum. The server owns the stream. Route handlers take `ParsedRequest` and return `Response` — they never touch `BufStream` / `MaybeTls`.
- **Crypto provider is `ring`, selected explicitly** via `builder_with_provider` (no process-default; aws-lc-rs is not in the tree).

## Attention System

### How It Works
Agents signal they need user input via Claude Code hooks configured in `.claude/settings.local.json` (written by `inject_attention_hook` in `agent/spawn.rs`): a catch-all `Notification` hook (permission prompts, idle prompts, elicitations) plus a `Stop` hook (turn ended). Both run the same curl command, which forwards the hook's **stdin JSON** as the POST body (issue #878):

```
curl -sf -X POST -H "Content-Type: application/json" --data-binary @- http://localhost:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID || true
```

The hook reads `$BUILDMESH_PORT` (set per-agent in `spawn_environment`) at run time rather than baking a literal port, so it routes correctly across the 1992→1994 fallback and to the dev profile's 2992 when an agent is spawned by `buildmesh-dev`.

**Codex is a different contract.** Codex's hook runner already wraps the command in `cmd.exe /C` (Windows) or `$SHELL -lc` (Unix), then `env_clear()`s down to a Core inherit snapshot. `BUILDMESH_PORT` / `BUILDMESH_SESSION_ID` are not in that snapshot, and a nested `cmd.exe /c "curl ... %BUILDMESH_PORT%"` never expands and never sees stdin. `provision_attention_hooks` takes the Buildmesh `node_id` as its own argument (not a field on `LaunchRuntime`) and bakes `http://localhost:<port>/api/attention/<node-id>` into both `command` and `commandWindows`, discards the HTTP response body, emits `{}` on stdout for Codex's Stop output contract, and installs `SessionStart`, `Stop`, `PermissionRequest`, `UserPromptSubmit`, `PreToolUse` for `request_user_input`, catch-all `PostToolUse`, and `Interrupt`. The callback is best-effort: curl errors are suppressed and the hook exits successfully if Buildmesh is unavailable or the node is stale. SessionStart is capture-only (`Decision::Ignore`): it persists `cli_session_id` from the hook payload but must not publish Ready or trigger Circuit observation or naming. Project trust (`ensure_codex_project_trusted`) is still required; `--dangerously-bypass-hook-trust` only skips hook-definition review. Resume argv is `codex resume [OPTIONS] <uuid>` — flags after the UUID are the optional prompt — modeled as `SpawnRecipe.base_args` (options) + `trailing_args` (session id, then prefill). The rollout poller (`services::codex_session`) remains the disk fallback and ignores `thread_source: subagent` files so a child thread cannot steal the stored id. Two Codex nodes must not share one worktree: `hooks.json` is last-writer-wins on the baked node id.

`POST /api/attention/{session_id}` normalizes harness callbacks before publishing
through `node_turn` and `SessionLifecycle`. A clean completion lands in `Ready`,
a structured question or permission lands in `AwaitingInput`, and known pending
work stays `Running` with a `BackgroundRunning` observation. The lifecycle owner
commits status, observation timestamp and snapshot together before desktop/mobile
events or naming/autopilot consumers run. Node list reads restore that snapshot;
legacy attention-clear events do not carry authority to change client status.
See [Agent Node status observation](development/node-status-observation.md) for
ordering, health, recovery and harness capability limits.

### False-Yield Suppression (issue #878)
Claude Code ends its turn when it launches background work (`run_in_background` Bash, timeout-backgrounded commands) and re-invokes itself when the `<task-notification>` arrives — so a Stop (or 60s-idle Notification) is *not* always "the user is needed". The route reads `transcript_path` from the hook payload and asks `transcript_reader::count_pending_background_tasks` for launched-but-unnotified task IDs (launch = a `tool_result` promising "You will be notified when it completes"; finish = a `<task-id>` notification with a **terminal** status — `running`-status notifications don't count). Pending work → the Node Turn is published via `node_turn::publish_hook` with `background_running` (naming/autopilot still fire; no attention mark). Permission-prompt Notifications always mark, even mid-background-wait. Any unknown (empty/garbage body, unreadable transcript) degrades to marking — never to silence.

**Safety net:** `attention_autoclear.rs` arms on every mark; if the PTY then produces ≥512 bytes of output more than 3s after the mark with no user keystroke, the node flips back to `running` and `attention-cleared` is broadcast. The 3s grace absorbs the Stop-hook-vs-final-redraw race; the burst threshold ignores idle control-sequence trickle. This self-heals the cases the transcript scan can't see (hook-less providers, format drift, lost notifications). Every path that clears attention or accepts user input must call `attention_autoclear::disarm` (see `write_to_agent_blocking`, `http::ws`, `coordinator::drive`, `circuit::delivery`).

### Cross-harness hook normalization (2026-09)

All structured attention callbacks enter `http::routes::attention` and are
normalized into the shared `agent-lifecycle` kinds. A clean `Stop` or native
idle event is `turn_completed`/`ready`; a permission or question callback is
`permission_requested`/`question_requested` and remains outstanding until its
matching resolution; input submission and permission resolution are
`work_resumed`/`running`. Errors, cancellation, malformed payloads, and
unreadable transcripts are degraded review checkpoints, never successful turn
completion. Background work is published as `background_running` without
attention until its terminal callback arrives.

Harness wire validation and mapping live in
`http::routes::attention::normalizers`, with one module per wired hook harness.
Dispatch uses the resolved node's harness, never a claimed payload provider or
another harness's classifier. Each module selects the fields it validates;
unrelated harness metadata cannot invalidate its callback. Unknown harnesses,
unsupported events and malformed payloads report `signal_unavailable` with
degraded health and cannot mutate turn/question/child correlation state.
Compatible hooks share envelope and tool mechanics, while event vocabulary and
observation strategy remain harness-owned. Explicit hooks are interpreted first;
only that harness's completion path may request transcript reconciliation.
Transcript reads run on the blocking pool after HTTP security checks, without
holding a database connection.

The route retains node/session ownership checks and delegates per-node callback
ordering to `attention::ordering`. Normalizers return observations and never
write node state. `node_turn` publishes the normalized lifecycle kind through
`SessionLifecycle`, then considers renaming only after an accepted lifecycle
commit. Replay fixtures and their evidence limits are documented in
[`tests/fixtures/attention`](../src-tauri/tests/fixtures/attention/README.md).

The route keeps per-node ordering state and fences callbacks by provider turn
id/session id. Foreground activity is tracked separately from outstanding
questions, so a delayed background callback cannot publish `ready` in the
middle of a live turn. This matters for Kimi Code: a background
`AskUserQuestion` returns before its answer, so `PostToolUse` is correlation
only; a later `Notification` with `source_kind=background_task` and a terminal
`task.*` type resolves it after the foreground `Stop`. OpenCode question and
permission events are tracked by request id (with a conservative single-
request fallback), and child sessions cannot overwrite their parent. Codex has
no permission-result hook, so its tool result or identified terminal `Stop`
resolves the attention route's approval marker. Circuit evidence applies a
stricter request identity contract: a permission callback without a request ID
retains an unresolved permission wait, and neither a generic Stop nor an
unrelated tool result resolves it. Native hooks are
provisioned only where the installed harness contract is verified; Terminal,
Freebuff, and unvalidated DeepSeek profiles retain explicit capability gaps
rather than guessing from PTY output. MiniMax's Agent-Plugin attention hook is
live: `requires_attention_hook` is `true` after the issue #1797 validation
delivered `Stop` from the installed 0.4.12 TUI. SessionStart is provisioned
for identity capture, but delivery from that TUI is unvalidated; live Stop also
delivered the session id after a completed turn. mcode 0.4.0+ reads
`.claude-plugin/plugin.json` with `hooks` **inlined** (a separate
`hooks/hooks.json` document is ignored, and a directory with no manifest is
skipped silently), runs `command` + `args` with no shell interpretation, and
`env_clear()`s `BUILDMESH_*` — so the shared callback URL bakes only the port. Native session/workspace payloads resolve the node; the provisioned SessionStart handler is capture-only, but its delivery has not been observed live.
Only `TurnCompleted` is advertised: the launch pins Full Access
(`permissionMode: bypassPermissions`, a surgical one-line edit to
`<dataDir>/config.yaml` — the TUI has no permission flag), so no permission
signal is claimed. Its
`messages.jsonl` transcript is wired too (`TranscriptFormat::Mcode`); see
`docs/learning/mcode-harness-capabilities.md`. Muse's interactive TUI exposes
no hook/event flag, so its turn signal comes from `services::muse_watcher`,
which tails the durable
`~/.local/share/muse/sessions/…/session.jsonl` run boundaries (`runtime.session`
records with `payload.kind == "run"` and `event.kind == "terminal"`) and
publishes each as a Node Turn — a passive watcher like Command Code's, with
`requires_attention_hook = false` and `attention_capability = None`. (Muse 1.3.0
does ship a claude-compatible plugin hook surface, but it is gated behind an
explicit `muse plugins approve` into a global plugin cache, and the node-local
`--scope project` install is refused until the workspace is trusted — issue
#1706 — so it is deliberately not provisioned; see
`docs/research/muse-attention-signals.md`.) Because
Buildmesh launches `muse --disable-approval`, a `PermissionRequested` signal is
impossible by construction and is not classified. Muse is additionally the only
harness shipped with its own always-on OS sandbox: Buildmesh bakes
`--disable-sandbox` next to the approval flag so the agent shell reaches the OS
credential store gh and git credential helpers resolve GitHub auth from (issue
#1788). See
`docs/learning/harness-attention-reliability.md` for the evidence matrix and
remaining limitations.

### Auto-Spawn Behavior
`AgentTerminal` component auto-spawns the agent when mounting an agent node with `status === 'idle'` and a `provider`. It uses `fitAddon.proposeDimensions()` to get PTY size before calling `spawn_agent`. This couples terminal mount directly to agent spawn — debugging attention issues requires tracing this path.

## Agent Node Management

### Agent Node ID Capture
Session IDs are **assigned, not captured**, for providers whose CLI accepts a caller-chosen id (Anthropic): the orchestrator mints a UUID up front, writes it to `agent_nodes.cli_session_id` *before* launch, and passes it via `--session-id <uuid>` (`agent/spawn.rs`, `SessionIdMode::Assign`; ADR 0024). The PTY reader thread's labeled-UUID sniff (`session_capture.rs`) runs **only** for self-assigning providers that print a UUID banner (Codex, Antigravity) — gated by `reader_should_capture_session_id` / `captures_session_id_from_pty` so there is exactly one writer per spawn (issue #651). Recent Codex TUIs often omit the banner: SessionStart (hook payload) and the rollout `session_meta` poller (`after_fresh_spawn`) are the load-bearing capture paths; PTY sniff is opportunistic. Antigravity also self-assigns UUIDs without printing them: `services::agy_session` scans transcripts past the mtime gate, reads matching launch `workspace_uris` from the sibling read-only `conversation_summaries.db`, and uses a single unambiguous decodable file root as an anchor when its path form matches the spawn directory; multi-root rows fall back to transcript Cwd. WSL guest paths and host UNC paths are not reconciled here; transcript Cwd follows the same limitation. Missing or unreadable summary data, blank rows, rows with no decodable root, and ambiguous multi-root rows fall back to transcript Cwd; malformed metadata for that conversation remains unverified. OpenCode also self-assigns, but its ids are `ses_…` (not UUIDs) and are not printed on the TUI: a fresh spawn uses `SessionIdMode::None` and `OpenCodeAdapter::after_fresh_spawn` reads the local `opencode.db` SQLite store (`services::opencode_session`) for a row created in the spawn time window whose `directory` matches the node; resume is `--session <id>`. MiniMax Code (`mcode`) also self-assigns: Buildmesh provisions SessionStart capture, but delivery from the installed TUI is unvalidated. Live Stop callbacks do deliver the session id and can capture identity after the first completed turn. Until a callback arrives, a new node has no stored session id; the removed time-window manifest poller has no verified replacement. The callback route binds a unique live node by conversation id and workspace (`services::mcode_session`), with a conditional write fenced by provider, workspace, process generation, and duplicate ownership. PTY sniff remains off. Don't replicate any of these paths — they are backend-only. `CLAUDE_CODE_SESSION_ID` is deliberately **not** used: Claude Code sets its `CLAUDE_CODE_*` vars *downward* into its own subprocesses, so a parent that spawns `claude` can't read it, and for Claude we already know the ID (we assigned it). See ADR 0024 and `docs/learning/opencode-harness-capabilities.md`.

### Turn Counting and Node Naming

**Background inference** is an adapter capability, independent of interactive
prefill, turn hooks, transcripts, and resume. `AgentProvider::background_recipe`
returns both the invocation and its `BackgroundInferenceCapability`; the
generated catalog and settings pickers derive eligibility from that recipe.
`agent::background` owns launch validation, prompt transport (stdin, argument,
or file), authentication environment, and final-answer extraction (stdout,
result file, or structured events). Naming and Circuit classifiers share it.
Unknown harness identifiers are rejected instead of using the legacy database
parser's Claude fallback. Both consumers own descendant cleanup through
`BackgroundProcessGuard`; callers supply isolated directories and execution bounds.
Claude command resolution preserves PATH lookup and Windows native/npm install
fallbacks for both binary spellings, unless an executable override is supplied.
Extra CLI arguments are rejected because they can change this protocol;
provider routes require explicit support from the background recipe. Adding a
runner belongs in its adapter, without an orchestration allow-list. Naming
preflight failures release in-flight ownership without consuming an inference
attempt or discarding the buffer, so repaired settings can retry on a later turn.
`session_naming.rs` captures PTY output and auto-names agent nodes via LLM summarisation (slug-based, e.g. `fix-auth-flow`). Buffering is gated: `on_output` only starts collecting after the first `on_turn` (first idle-prompt webhook) fires, so the Claude Code startup chrome — banner, "Bypass Permissions" warning, plugin/skill listing — is discarded before it can reach the LLM. The rename runs async one turn later, against clean post-startup content.

**Name uniqueness is load-bearing for Worktree Nodes.** A Worktree Node's `name` is also its `worktree_name`, its worktree directory, and (branched mode) its local branch — see `services/agent_node.rs` (`worktree_db_name = session_name`). Issue/PR spawns derive that name deterministically (`issue_node_name` / `pr_node_name` → `gh{N}-{slug}` / `pr{N}-{slug}`), so a second spawn for the same issue or PR derives the same worktree path as the first. `git::worktree::provision_for_spawn` cannot recover from that: its warm path refuses the adoption because the branch is already checked out there, and its cold path's path-exists short-circuit hands the same directory to both nodes. (The warm-failure cleanup used to read "path exists" as "our move created it" and delete the other node's live worktree; it now only removes a target it created itself.) Any new spawn source that derives a name deterministically must therefore make it unique per Mesh — `session_naming::disambiguate_node_name` is the helper, and the PR pill's reviewer spawn (`create_pr_node` with `reviewer`) is the worked example.

### Profile ownership — one process per app-data profile (issue #1521)

Exactly one Buildmesh process may own an app-data profile, and the claim is taken in `setup` before `db::init` and before any worker starts. `instance_guard::with_profile_ownership` owns the ordering: everything that touches the profile lives in `run_profile_startup`, the continuation it only runs for the winner, so a second launch cannot open the database. That is the whole fix — the damage in a two-process profile was never the duplicate window, it was the loser's startup crash sweep running against a database full of live nodes it cannot see (`ProcessRegistry` is process-local), marking the winner's running Agent Nodes suspended so the frontend can resume a second harness in the same Worktree Node.

The claim is keyed on **bundle identifier plus canonicalised app-data dir**, so the stable hub and the `.dev` profile both run at once (they already differ in `http::port_offset` and in their data dir), and so two spellings of one path are one profile. Mechanism: a Win32 named mutex in the `Global\` namespace, created *without* initial ownership on Windows — `Global\` rather than `Local\` because a session-scoped object is invisible to a second Windows session, and an RDP reconnect or a scheduled task resolving the same profile must not become a second primary — the object's existence plus our open handle is the claim, so there is no release step to leak — and an exclusive non-blocking `flock` on `instance-<digest>.lock` in the profile dir elsewhere, where the digest is the same (identifier, profile) fingerprint. Both are released by the OS when the process dies, which is what lets the crash watchdog relaunch into a profile it can still claim.

A losing claim is retried for a moment before it is treated as a second launch, because Tauri spawns an update or restart successor from inside the *outgoing* process's `Exit` event: without that wait a relaunch would hand its activation to a process that is on its way out and leave the user with no app at all. A genuine losing launch restores and foregrounds the owner's window on Windows, appends a line to the profile's `logs/profile-ownership.log`, and exits 0; there is no portable window handle elsewhere, so macOS and Linux exit quietly and the user switches to the running window themselves. The lookup is by **process id**, not window handle: the owner publishes its pid in `instance-owner.pid` the moment it wins the claim, and the loser enumerates top-level windows for that pid, matching the one whose title starts with the shared `MAIN_WINDOW_TITLE_PREFIX` (`Buildmesh - `, set by `lib.rs`). A handle would have been the obvious choice and is the wrong one — `WebviewWindow::hwnd()` answers `RawHandleError(Unavailable)` while `setup` runs on Windows, because the native window is only created once the event loop starts, so a handle protocol has a hole in it that only shows up on a real second launch. A profile that cannot be resolved or claimed is a fatal, *visible* startup error (native message box, plus the same log), never permission to continue: an app that cannot say which profile it owns must not open that profile's database. `InstanceGuard` takes no `tauri::App` and no window, which is what lets a child-process test run the real gate — with a real `db::init` as its continuation — and prove the loser never reaches database initialization.

### Crash Recovery on Startup
`session_lifecycle::recover_from_crash()` (called from `lib.rs` setup) marks any agent nodes still showing `Running` status as `Suspended` during app startup, since a crash means no live process exists. These are then auto-resumed via `auto_resume_nodes` on the frontend's first draw. A second sweep, `session_lifecycle::on_exit_sweep()`, runs from the `RunEvent::ExitRequested` callback to handle the graceful-shutdown case the same way; both wrappers live inside the `SessionLifecycle` module so the "exactly one place writes `agent_nodes.status` for suspend sweeps" invariant holds (issue #949, issue #132). The premise "a crash means no live process exists" is now enforced rather than assumed: the sweep sits behind the profile-ownership gate, so only the process that owns the profile reaches it (issue #1521).

### Startup readiness contract (issue #1524)
The window reaches the workspace only after the boot sequence returns a clean verdict, and `src/lib/bootSequence.ts` owns that contract. It runs the boot loaders concurrently and treats a failure as either a *rejection* or a *stored store error*. The second channel is not belt-and-braces: `meshStore.fetchMeshes` and `agentNodeStore.fetchAgentNodes` absorb their IPC failure, write `state.error`, and resolve, so `Promise.allSettled` alone reported a successful boot for a workspace that never loaded (it painted an empty workspace, which reads as data loss). Boot therefore uses the rejecting `meshStore.refreshMeshes` for the Mesh snapshot and cross-checks *both* stores' `error` fields once the loaders settle; both stores clear `error` when a load starts, so a non-null value belongs to the attempt that just ran. `isReady` is set only on a clean verdict, and every failure reaches `<BootErrorPanel>` as one `Source: message` line so the panel's **Retry** can re-run the load.
`agentNodeStore.initAttentionListeners` is the other half: listener registration is a three-state machine (`idle -> attaching -> attached`) with one shared in-flight promise, so React StrictMode's double-mount cannot register an event twice. `attached` is reached only after every `listen` resolves, and a mid-sequence failure makes `agentNodeListeners.attachAgentNodeListeners` roll back the already-registered handles before the store drops back to `idle`. The rollback isolates each unlisten handle: one that throws must not strand the remaining handlers, and must not replace the registration error Retry needs to report (it is logged and the original error is rethrown unchanged). The earlier boolean flag was set *before* the await, which is why a failed attachment was unrepairable: **Retry** short-circuited on the flag and the store stayed deaf to lifecycle events. Do not trade the state machine back for a "have we started?" boolean; the store has no unmount that could clean up a partial attachment, so a half-wired bus would be permanent.
### auto_resume_nodes
On app restart, the frontend calls `auto_resume_nodes` which iterates all `Suspended` agent nodes with a `cli_session_id` and calls `spawn_agent_inner` with `SessionIdMode::Resume`. Whether a harness participates is `AgentProvider::auto_resume_on_startup()` (true for Anthropic, Codex, Cursor, OpenCode, Kimi, and others that opt in). A harness that returns false is left `Suspended` (`decide_startup_resume` → `SkipAdapterDeclines`) so the user can Resume / Regenerate from the UI.

### Early-Exit Detection
The PTY reader thread records `spawned_at`. If the reader exits within 3 seconds, the agent node is marked `Error` and a `resume-failed` event is emitted. This catches failed `--resume` attempts where the agent CLI exits because the session has expired.

### Expired Session Recovery & Start Fresh (issue #1306)
When a node transitions to `Error` status after an expired or invalid session ID fails to resume, `session_lifecycle::on_resume_failed` marks the status as `Error` but intentionally leaves `cli_session_id` intact (since `on_resume_failed` is a status-only writer, preserving the ID for transient-failure retries).

To break unrecoverable restart loops where an expired session ID would otherwise be retried indefinitely:
- **Retry Resume (`↻` inline button):** Re-attempts spawning via `spawnAgent(node.id, node.provider)` with the existing `cli_session_id` (`SpawnIntent::Resume`) for transient failures (e.g. network blips or race conditions).
- **Start Fresh (Context Menu item):** Invokes `restartFreshAgent(node.id)` which calls `spawn_agent` with `resume = null` (`SpawnIntent::Fresh`). The backend `spawn_with_intent` pipeline detects `intent_replaces_conversation(&intent)` and executes `db::clear_cli_session_id(node_id)`, resetting `cli_session_id` to `NULL` in SQLite and launching the agent fresh in the existing worktree with correct terminal dimensions.

### `AgentNode.branch` is overloaded (base ref vs PR head ref)
The `branch` field on `AgentNode` (see Rust doc comment at `src-tauri/src/models/mod.rs`) means two different things depending on spawn source:

- **Issue-spawned, hand-spawned, and handover-spawned nodes** — `branch` holds the mesh's `base_ref` (resolved via `commands::git::get_default_branch`, typically `origin/main`).
- **PR-spawned nodes** (issue #420, where `source_pr.is_some()`) — `branch` holds the PR's `head_ref` instead. The worktree is cut from `origin/<head_ref>` (or `fork-<owner>/<head_ref>` for fork PRs, issue #443) so the agent lands on the same commits the PR is built from.

**Disambiguator:** `source_pr.is_some()`. When set, treat `branch` as the PR head ref; otherwise as the mesh's base ref.

**Canonical reader:** `provision_workspace` in `src-tauri/src/agent/spawn/provision.rs` (called from `spawn_agent_inner`) derives the actual worktree `base_ref` from this field — see the `worktree_base_ref` block that keys off `node.source_pr.is_some()`. New code that needs the worktree's base ref should NOT reimplement the overload — call into that resolution or use the same `if source_pr.is_some()` pattern. The `commands::agent::create_pr_node` row-creation comment ("the *row* (`source_pr` is set, `branch` is the head ref) and in stage-2's `git fetch origin <head_ref>` worktree adoption") is the other half of the contract: the write side chooses the head ref precisely so the read side's `if source_pr.is_some()` switch lands on the right branch.

## Agent Process Architecture

### ProcessRegistry — Runtime State
Agent state lives in a **static** `ProcessRegistry`: `HashMap<i64, Arc<AgentProcess>>` using `once_cell::sync::Lazy`. The DB is **not** the source of truth for running agents — it's only used for `cli_session_id` persistence across restarts.

### AgentProcess Fields
Each entry holds:
- `child` — `Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>`
- Writer channel — takeable internally so teardown can close it before joining the writer thread (issue #1531). Callers enqueue through `write_bytes`; a live sender plus a `recv()`-blocked writer pays the two-second join fallback.
- `master` — `Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>` — `take()` on kill/EOF closes the ConPTY (issue #300)
- `reader_alive` — `Arc<AtomicBool>` — set to `false` on PTY EOF; used to detect if an agent is still alive
- `generation` — per-incarnation token assigned at `insert`. `kill_session` and the reader-EOF `reap_incarnation` compare-and-remove against it so an old EOF cannot delete a replacement process (issue #1531)
- `job` — `Option<process_util::JobHandle>` — a Windows Job Object containing the agent's whole process tree (see *Killing the process tree* below); `None` on non-Windows or if assignment failed

The PTY handles are behind `Arc<Mutex<...>>` so the PTY reader thread and Tauri command handlers can both access them safely.

### Killing the process tree (Windows)
`kill_session` must kill **everything** the agent spawned, or a survivor pins the worktree's directory (as its CWD or via an open handle) and blocks removal on close. `taskkill /T` alone is insufficient: it walks *live* parent→child links, so it misses any descendant whose parent already exited — e.g. a dev server the agent backgrounded then orphaned. The fix is a **Job Object** (`process_util::JobHandle`): at spawn we assign the PTY shell to a kill-on-close job, so every process it later spawns is *contained* however it detaches. `kill_session` calls `TerminateJobObject` first (reaches detached/orphaned descendants), then keeps `taskkill /T` + `child.kill()` as fallbacks for the rare case job assignment failed. Assign happens immediately after spawn, before the shell launches the agent CLI, so the whole tree is covered. FFI to `kernel32` is declared inline via `extern "system"` (same no-new-deps pattern as `services::usage.rs`).

Teardown is centralized (issue #1531): compare-and-remove (or `kill_session`'s take of the current entry) claims exclusive ownership of the `Arc`, then cancel input (`close_input`), close the master, terminate/reap the process tree, join worker threads, and revoke Windows restricted-token grants. Natural PTY EOF goes through `reap_incarnation` with the same compare-and-remove so an idle node does not keep a registry slot, writer thread, child handle, or sandbox ACE. When `insert` overwrites a previous incarnation, it runs the same handle/thread teardown but **skips** sandbox revocation — grants are keyed by `session_id`, and the replacement spawn already registered them before `insert`. `kill_session` is the only public way to drop a live entry; an unconditional `remove` would delete a replacement spawn. Node-scoped output Channels stay registered across process incarnations.

### Worktree Support (git2-based)
Buildmesh creates a dedicated worktree per agent node **itself**, via `git2` in `git/worktree/mod.rs` (`create_git_worktree` → `add_worktree_impl`) — for **all** providers, not just cwrap. This prevents concurrent agent node conflicts when multiple agent nodes target the same git repository. Two modes: `branched` (default, a real branch per worktree) and `detached` (a throwaway detached HEAD); both are cut from the configured Base Ref (default `origin/main`), resolved via `resolve_base_commit` with a fall-back to local `HEAD` when the ref is unresolvable (#230). See `docs/adr/0003-buildmesh-owns-worktree-creation.md` for why this moved off the agent CLI's old `-w` flag, and `docs/adr/0007-extract-git-module.md` for why the worktree lifecycle now lives in the `git` module.

**Worktree Provisioner** (`git/worktree/provision.rs`) is the four-branch decision that turns a Spawn Context into an on-disk worktree (`Reused` / `Adopted` / `Upgraded` / `Created`). The spawn pipeline (`agent/spawn/`) is four phases coordinated by `spawn_agent_inner` (issue #1427): prepare returns **workspace params** (git/disk) and **launch params** (PTY size, prefill, cascade overrides) as separate values; provision takes only workspace params; launch takes the provisioned workspace *plus* launch params; streams register the process. Do not courier launch knobs through the provision DTO — git worktree provisioning does not care about terminal columns. `spawn_agent_inner` acquires the in-flight claim as a named local before the phase calls (never a discarded `_claim` tuple from prepare — that reopens the #650 race). `spawn_with_intent` is the sole owner of `node-spawn-failed` and session-lifecycle Error writes; phase modules return `Result` so Resume can still call `on_resume_failed`. Prepare reads the mesh/node/warm-pool rows into `WorkspaceToProvision`. Provision builds a `SpawnContext` from those fields, hands it to `provision_for_spawn`, and lets the provisioner own post-spawn bookkeeping (forget-after-spawn, name adoption, status writes) internally — a provision `Err` is returned to the orchestrator, not handled here. Base-ref resolution (`SpawnContext.base_ref`) reaches the cold-path `add_worktree_impl` end-to-end via this seam (pinned by `provision_for_spawn_cold_created_uses_spawn_context_base_ref_not_local_head` for issues #230, #248). `.worktreeinclude` is applied by `apply_worktree_include` (`git/worktree/mod.rs`) on every cold-path `Created` and warm-path `Upgraded` outcome — including **recursive directory copy** as of #248 (previously log-and-skipped; pinned by `apply_worktree_include_copies_directory_recursively`).

**Resume:** worktrees are created only when the directory does not already exist (the `if !host_path.exists()` guard in `agent/spawn/provision.rs`), so resume simply re-spawns inside the existing worktree — no re-creation, and none of the old `-w` "already checked out" failures.

**Auto-sync on spawn (issue #213, relaxed by ADR 0020):** before creating a *new* worktree (resume doesn't re-sync), `git::sync::fetch_origin` runs `git fetch <remote>` + `git pull --ff-only --no-rebase` on the parent mesh. The `--no-rebase` is required to defeat a global `pull.rebase=true` config — a rebase on a diverged history would write conflict markers to the working tree, silently mutating the user's local branch on what's supposed to be a read-only step. The sync is best-effort: dirty parents, no-origin repos, and already-up-to-date branches are silent; a fetch failure, diverged history, or unreadable repo surfaces a `mesh-sync-warning` toast (frontend label: `Sync`) and spawn proceeds from local HEAD. See `docs/adr/0001-auto-sync-mesh-on-node-spawn.md` and [[buildmesh-pull-rebase-default]].

**Spawn-time fetch TTL + background mesh sync (ADR 0020):** the spawn-time auto-sync is SKIPPED when the mesh was successfully synced within `services::fetch_freshness::SPAWN_FETCH_TTL` (5 min) — the background worker (`services::pool_worker`) re-fetches every idle worktree-enabled mesh once per `BACKGROUND_SYNC_INTERVAL` (3 min, gated on last *attempt* so an offline machine doesn't hammer retries) and triggers `warm_pool::on_fetch_completed` when the ref advances, so both the mesh and its warm pool stay continuously fresh without a network round-trip on the click-to-terminal path. All fetch paths stamp the registry via the `locked_*` wrappers in `git::sync`; the PR-head fetch is never skipped (correctness, not freshness); the manual Sync command is the "latest right now" override. Do NOT add a new fetch call site that bypasses `locked_fetch_origin` / `locked_do_sync` — it would fetch without stamping freshness and reintroduce redundant spawn-time fetches.

**Warm pool default (schema v24, ADR 0020):** `pre_spawn_pool_size` defaults to `1` (pool ON) for new meshes, with a one-time flag-gated backfill (`pool_default_backfill_v24`) for existing worktree-enabled meshes. Opt out per mesh via the Worktrees Probe.

**Close/removal (optimistic + deferred):** closing a node is split in two. Phase 1 (`services::agent_node::delete`) kills the process tree (via the Job Object — see *Killing the process tree* above, so a dev server the agent spawned can't keep the directory pinned) and, in one transaction, deletes the `agent_nodes` row *and* enqueues the worktree into `pending_worktree_removals` — fast and authoritative, so the UI drops the node at once. The slow recursive directory delete (`remove_one_worktree`) runs as a background drain (`process_pending_removals`) that dequeues only on success; an app quit mid-cleanup is resumed by the startup reconcile in `lib.rs` `setup()`. Net: "node gone from UI" no longer implies "directory gone" — close is eventually-consistent on disk, and a stuck removal raises a `worktree-cleanup-failed` toast. See `docs/adr/0004-optimistic-node-close-deferred-worktree-removal.md`.

## Legacy automation upgrade compatibility

Legacy Autopilot is removed from the application. Only `db::legacy_retirement` and `services::legacy_retirement` handle the one-way upgrade and retry cleanup of persisted legacy nodes. Existing nodes, worktrees and history remain available; legacy columns and ledger rows are never launch configuration or capacity inputs. Retired configuration is not loaded into Circuit settings. Persisted `AutopilotCircuit` names and graph keys belong to Circuit schema compatibility, not the legacy runtime.

## Autopilot Circuits (spec #1205, walking skeleton #1206)

The application automation runtime is a composable trigger-action graph. A **Circuit** is a blueprint DAG (`circuit::model::CircuitGraph`, serialised as the `graph_json` TEXT column — no per-node-kind migrations, the AST evolves inside the JSON); a **Circuit Run** is one execution (`autopilot_circuit_runs`); a **Circuit Step** is per-circuit-node state within a run (`autopilot_circuit_run_steps`, schema v34). "Node" is overloaded everywhere: *circuit node* = graph vertex, *agent node* = mesh session.

- **Pure core, thin impure seam:** `stepper::advance(run, event) -> Transition {step_writes, effects}` never touches SQLite/PTY/clocks — every impure fact arrives as an event (`Tick(Capacity)`, `AgentFinished`, `AgentReady`, `AgentLost`). The seam is `services::circuit_worker`: a dedicated OS thread (2s fast tick + condvar wake for Trigger Now) that observes live state → steps → commits atomically via `db::commit_circuit_advance` (one transaction for run-state + all step upserts; `UNIQUE(run_id, node_id)` backs the upsert) → executes effects. GitHub effects live in `circuit_worker/github.rs`; spawn overrides in `circuit_worker/spawn.rs` — a new effect kind should not enlarge the loop.
- **Single vocabulary owner (issue #1660):** `circuit::vocabulary` owns `RunState` / `StepStatus` (`Queued` stores as `pending_slot`) and the terminal predicates. Persistence, the worker, and the UI import those tokens (generated `RunState.ts` / `StepStatus.ts` + `circuitVocabulary.ts`). Do not re-spell `pending_slot` or terminal sets at call sites.
- **Single capacity policy (issue #1660, ADR-0028 unchanged limits):** `circuit::capacity` owns admission arithmetic, Tick snapshots, and `CapacityBind`. The worker observes counts then calls `may_admit` / `tick_capacity`; the Probe's `queuedReason` renders the bind. Do not re-derive slot math in the UI or worker.
- **Trigger dedupe lives in the schema**: `UNIQUE(circuit_id, trigger_identity)` + `INSERT OR IGNORE` replays the existing run id. Circuit-scoped, so two circuits may react to the same source independently.
- **Concurrency:** the circuit and legacy Autopilot policies are separate (issue #1467, ADR-0028):
  1. **`meshes.autopilot_concurrency_limit`** is the legacy Autopilot agent-node cap. The circuit worker does not read it; changing the legacy "Max concurrent autopilot nodes" setting must not change circuit-run admission or circuit fan-out.
  2. **`meshes.circuit_run_capacity`** (schema v36) bounds the **admitted circuit runs** on a mesh (`db::count_active_circuit_runs`, count = `running + paused`). One slot per admitted run regardless of fan-out agent count. Default `2`; range `1..=8` validated at the IPC boundary (`update_mesh_circuit_run_capacity`). The worker reads this column in `run_pass` to gate the `pending → running` flip (`may_admit_run`); a `pending` run stays in DB `state = 'pending'` (NOT a step status) until a slot frees.
  3. **`autopilot_circuits.concurrency_limit`** is the per-circuit step budget, distinct from both mesh settings. It bounds running *steps* within a circuit's `running + paused` runs (`db::count_running_circuit_steps`) and drives `stepper::schedule_ready`.

  Circuit runs reserve their declared SpawnAgentNode footprint in a durable run lease, and those leased slots are also counted against the optional app-wide `circuit_agent_pool_size` backstop. This removes the partial-admission deadlock (runs 3+4 of circuit 5) without depending on retired legacy mesh settings; if the optional global pool cannot fit a blueprint's footprint, that run remains pending. With no global pool configured, there is intentionally no additional per-mesh agent-process cap: `circuit_run_capacity` limits admitted runs, not their fan-out. Admission uses worst-case durable lease reservations, while a running Tick uses live circuit-agent counts for the same host-wide pool. Capacity snapshot failures fail CLOSED (zero capacity), loudly logged.
- **Observation preflight and scheduling:** terminal protocol responses (including focus and cursor-position reports) are distinct from prompt edits; only actual input changes its generation/draft fence. Before inference, an unusable report binding becomes a typed, persisted observation blocker. Native finished-turn report snapshots can establish readiness despite stale display status, without inventing background ownership. Classifier and verification jobs run in a bounded FIFO pool (four active, 128 queued), deduplicated by gate/attempt; the worker consumes results with cancellation and freshness fences. Verification worktree paths are exclusive. No DB connection or global registry lock spans job execution. See [session observation](development/circuit-session-observation.md) for the contract, evidence requirements and remaining fleet-scale work.
- **Projection and delivery reconciliation:** a resumed process may replace status-only evidence from its previous incarnation, but native evidence and conflicts of unknown provenance remain blockers. Legacy projection conflicts require their exact original history digest before reconciliation. Automated Muse follow-ups in an established session require a matching newly appended native run-start record; a terminal redraw or pasted-content box cannot acknowledge submission.
- **Circuit completion policy:** `circuit::observation` retains identity-scoped foreground, owned-work and report evidence. Native lifecycle verification remains distinct from report-based progress. A yielded agent may advance from a stable transcript report bound to its session incarnation, input stamp, report revision and publication time; `transcript_reader::report_snapshot` rejects newer tools/input and reuses native turn tracking where available. Commit revalidates the report before taking the DB writer, then fences input/session under the existing transaction boundary. Known unfinished work, evidence conflicts and native human requests block report routing. A generic AwaitingInput projection is a yield, not an indefinite request. Unverified agent steps remain observable for fresh reports and confirmed loss/error; ambiguous external effects never replay automatically.
- **Prompt injection rides the existing two-phase discipline:** the canonical blueprint spawns fresh and delivers the prompt via an InjectPty step gated on `PROCESS_REGISTRY.is_alive` (`AgentReady` event); hand-authored spawn prompts stage as prefill via `SpawnIntent::Loop`. Never bypass `circuit::delivery::write_prompt_to_pty`. Interactive callers use the same seam — the terminal's **Handover to node** hands a selection to an existing agent — and its submit posture is keyed to whether the evaluator *buffers* the target (`circuit::evaluator::is_circuit_piloted`), deliberately not to whether the target has produced output yet: a target no one registered (any node a human spawned) settles on a fixed window and gets exactly one Enter, with no acknowledgement retry and no attention fallback, because no signal exists for it, while a buffered-but-still-silent node — the state Circuit prefill delivery starts in — keeps the retry ladder.
- **Milestone 3 — circuits react to the world (#1208):** `services::circuit_triggers` owns run *starts*: a GitHub poll pass (every 120s, `maybe_poll_github`, on-demand via `request_github_poll`) ingests labelled open issues/PRs per enabled circuit (`issue:<n>:<label>` / `pr:<n>:<label>` identities — dedupe stays schema-scoped), and the interval pass fires Interval circuits off a cooldown anchored on `MAX(created_at)` of their runs. Trigger sources seed the run's context (`issue.*` / `pr.*`, built by `CircuitContext::with_issue`/`with_pr`), so templates resolve at node execution time. `GithubAction` nodes are instant-completing steps handing an `Effect::CallGithub` to the seam, which resolves owner/repo from mesh origin + target number from context and calls `GitHubClient`; a failed HTTP call fails the step/run loudly. Resilience: `startup_reconcile_pass` runs once per launch and closes the observation-invisible wedge (a Running spawn step with no attached agent = commit-crash gap → fail; lost/archived agent or vanished worktree dir → Lost), while `lost_turn_watchdog_pass` inspects piloted agents quiet ≥60s still marked `running`, then routes a recovered turn through `commands::attention::mark_attention` only after readiness classification and freshness checks (see the quiet-turn boundary below).

  - **Issue-driven Circuit + PR review blueprint:** `CircuitBlueprintKind::IssueDrivenAutopilotReview` is server-owned and requires a GitHub issue-label trigger plus a PR-producing mesh policy. It reuses the issue-review Circuit finish prompt and collaborator gate, then requires the implementation agent's pushed branch/PR before spawning a separate reviewer. The reviewer uses `CircuitGraph::pr_review_prompt()` (the shared `REVIEW_POLICY` plus PR comment scope); it receives both `{{pr.number}}` and `{{pr.url}}`, its terminal report is persisted as `node.reviewer.output`, and the circuit injects that feedback into the implementation agent, which pushes its fixes to the PR. The reviewer is spawned once: the bounded `RetryLimit` re-enters at `re_review`, which re-prompts the same open reviewer, and a `review_round` join feeds either reviewer turn to `ReviewVerdict`, with `retry.*` and `verification.*` context populated as those gates run. Approval closes the reviewer, then `merge` asks the implementer to squash-merge the PR; the run completes once that prompt is delivered and hands the implementer back open. Blocked verdicts and exhausted review retries end upstream in `Failed` with a recoverable checkpoint, so they do not claim approval or erase the reviewer evidence. The reviewer has its own worktree and the circuit minimum concurrency is 2; its run admission is independent of the legacy mesh node setting, with only the optional app-wide Circuit agent pool remaining as a separate process-safety backstop. Mustache autocomplete and the inspector drawer only expose context paths reachable from the selected node's upstream graph.
- **Circuit-agent ownership:** circuit-spawned agents are registered in the shared evaluator but do not create `autopilot_runs` rows. The circuit worker owns their turn classification, per-mesh run and optional global-pool capacity accounting, GitHub-action replay after a restart, and best-effort cleanup when a run fails.
- **Quiet is not a finished turn.** The circuit lost-turn watchdog checks readiness before publishing a recovered turn: ongoing background work and unknown evidence leave the node running. It retries at most once per minute and rechecks lifecycle/input stamps, report revision, liveness, and quiet time after classification. This gate is separate from review verdicts, where `WORKING` means changes requested. Attention autoclear cannot undo a false turn after a circuit has already failed and stopped its agent.
- **Shared review contract:** the title-bar review preset and issue-driven review blueprint share `CircuitGraph::REVIEW_POLICY`, feedback instructions, and `ReviewVerdict` routing: approval closes the reviewer and asks the implementation agent to squash-merge before the run completes, findings start a fix round on the same reviewer, and an unclear verdict ends upstream in `Failed` with an attention checkpoint. Local review first asks its borrowed source to publish a PR and waits for that turn, then still inspects uncommitted work; PR review also posts findings to the PR. A completed run hands back every agent its graph did not close: it requests no cleanup lease, and the worker's terminal sweep stops piloting every agent the run referenced. Local source/fix handoffs wait for a finished turn, not proof of task completeness: a clean `Ready`/`Completed` state and fresh report bypass the task classifier, while ambiguous input waits use a review-readiness classifier. Circuit dispatch appends a versioned final-result contract to reviewer prompts (including re-review prompts to an open reviewer), node-review fix feedback and the local publish prompt. `circuit_worker::report_contract` parses one unquoted terminal result line; valid results bypass inference after the ordinary report/session/input preflight, including when the display is still Running. Duplicate, malformed or unknown-version contracts cannot approve; known child work and human requests still block. This establishes report readiness, never native lifecycle verification. A legacy free-form clean reviewer turn (`Ready`/`Completed` with a report) consults the live classifier first; only when that backend is absent or fails does the gate fall back to reading the report's explicit verdict deterministically — approval completes, requested changes start a fix round, blocked or ambiguous reports park for attention — so reviews complete on meshes with no classifier CLI (#1815). The worker uses the app-wide Reviewer provider setting when configured, then falls back to the reviewed agent's harness, while preserving explicit circuit overrides and the existing model/effort cascade. The selected reviewer provider is snapshotted into each run, so later settings changes do not mutate queued work. Reviewer parentage comes from the upstream implementation step or borrowed source and is persisted through `CircuitAgentOwnership.parent_node_id`, so both appear in the same node activity tabs. On blocked or exhausted review failure, cleanup stops the live reviewer process while retaining its identity, worktree, association, and report as a recoverable checkpoint; it does not delete the evidence. Startup upgrades exact stock defaults, preserves customized issue-review prompts/topology, and defers active graphs until a later startup.
- **OpenPr observes a worktree, not an agent process:** its repository seam resolves the upstream spawn association only to inspect the implementation worktree; it is excluded from PTY completion events, lost-turn watchdogs, and agent-lifecycle cancellation. The node carries an explicit `open_pr_policy` (`require_existing` for the review blueprint, `create_if_missing` for ordinary graphs; omitted is the backwards-compatible create default), so the generic action runner never identifies a blueprint by name. The v2 graph migration backfills `require_existing` on persisted review graphs that predate the field. The worker verifies local wrap-up prerequisites, then discovers the PR once using the worktree's actual checked-out branch (not its directory name). Lookup errors remain errors, not evidence that no PR exists; typed GitHub responses preserve PR number, URL, title, and head ref. Existing PR adoption populates `pr.*` before reviewer scheduling; agent completion alone cannot complete this gate.
- **Catalog & contract (#1469):** the shipped blueprint catalog is one canonical list at `circuit::blueprint_contract::BUILT_IN_CATALOG`. Every entry pins marker, allowed triggers, required node ids + edges with conditions, concurrency floor/ceiling, manual-trigger eligibility, and user-visible prompt fragments. The catalog reconciles spec #1205's "three presets" prose with the actual implementation: `WalkingSkeleton` covers *Issue-Driven PR Flow* (Manual / labelled GitHub issue / labelled GitHub PR trigger roots) and *Continuous Looping Pacer* (Interval trigger root) under one skeleton; `IssueDrivenAutopilotReview` is the *PR Adversarial Reviewer* preset. Adding a new variant to `CircuitBlueprintKind` without a `BUILT_IN_CATALOG` entry fails `built_in_catalog_covers_every_blueprint_kind` at `cargo test` time; the matching Probe UI entry fails `tests/unit/circuits-probe-catalog.test.tsx` until the `<option>` lands. Stepper paths are pinned per-blueprint in `stepper::tests`; worker-seam helpers (`observe_close_agent_retries`, `reconcile_spawn_step`) carry the matching walking-skeleton + review-blueprint coverage so the impure seam can't drift from the canonical graph topology.

- **Built-in local review configuration:** new runs pin graph JSON, behavior revision and resolved reviewer launch configurations before acquiring the database writer. Review again increases the failed run's round allowance and sets a review-only graph as the same Circuit Run's executable snapshot. Each replaced graph is archived in per-run snapshot history, preserving the original pinned graph and intervening review snapshots. The reviewer launch configuration stays frozen, and no implementation or publication step is replayed. An explicit extension can admit on a disabled issue-review Circuit, and its formerly owned implementation agent still counts against the optional global pool. Existing Continued Review circuits are consolidated under their original Circuit on startup. Later preference edits apply to future runs. The preset is read-only; its independent editable copy starts disabled with a Manual trigger and no transferred history.
- **Transcript-driven observer recovery:** active circuits recover session identity and restore missing Command Code and Muse passive watchers before waiting for a lifecycle yield (issue #1794 added Muse to the per-harness recovery dispatch). Attaching a circuit to an already-silent source starts its quiet-observation clock without inventing a turn/report boundary. Command Code native committed model messages are distinct from legacy split message records; late watcher replay publishes only the current terminal state, never a historical completion followed by newer work.

## Logging and Crash Handling

- Logs written to `buildmesh.log` via `tracing-appender`. The subscriber is installed by the startup
  bootstrap (below), before the database opens, and mirrors to stderr only until `promote()` runs at the
  end of startup, so a *normal* session is file-only while a failing one is on stderr as well
- Panic hook writes to `logs/panic.log` with thread name, thread ID, and full backtrace
- **`RUST_BACKTRACE=1` is the launcher's job.** `Backtrace::capture()` (lib.rs:364) reads the env var at runtime and returns the "disabled backtrace" placeholder when it's unset. All four launchers (`scripts/run.ps1`, `scripts/run-dev.ps1`, `scripts/run.sh`, `scripts/run-dev.sh`) set it before launching; `tests/unit/launch-script-backtrace.test.ts` pins the contract so a refactor can't silently drop the env var. Without it, the "full backtrace" bullet above is a lie — the file would have one placeholder line.
- **`panic.log` vs `panic_early.log`** — two hooks, two files (`lib.rs:41-128` + `lib.rs:348-382`). The early hook is installed in `run()` BEFORE Tauri setup so it catches panics during Tauri-init that the main hook (installed later in `setup()`) can't. Bundle-id is derived from the binary name (`buildmesh-dev.exe` → `com.alond.buildmesh.dev`), so dev-profile crashes don't pollute the stable hub's logs. Both hooks `flush()` + `sync_all()` because `panic = "abort"` kills the process via `__fastfail` before the OS file buffer flushes.
- **`panic.log` is invisible to `buildmesh.log` pattern scanning.** The main panic hook writes to the file + `eprintln!`s but never pushes to the tracing pipeline. `/verify`'s full-tier log-scan (issue #158) tails `panic.log` and `panic_early.log` separately and treats any new line as an unconditional fail; the `scripts/run-dev.ps1` and `scripts/run-dev.sh` launchers also fast-fail on the same condition so a panic-only crash can't masquerade as a successful launch.
- **`watchdog.log` is intentionally out-of-process.** On Windows, the main process starts the same executable in private `--buildmesh-crash-watchdog` supervisor mode. The supervisor opens and retains a handle to the exact parent process before setup continues, then records the OS exit code and expected-exit marker after the parent dies. It cannot use `tracing-appender` because that pipeline dies with the process it observes, so each forensic line is appended and `sync_all()`'d directly. The external supervisor is the sole Windows relaunch owner; the in-process `WindowEvent::Destroyed` path deliberately defers to it, avoiding duplicate launches across an unavoidably non-atomic process-spawn boundary. An unexpected exit relaunches Buildmesh under the shared 60-second `auto_relaunched_at` crash-loop guard. `CloseRequested` and `ExitRequested` write a per-run expected marker, while non-Windows retains the guarded in-process WebView relaunch fallback. Set `BUILDMESH_DISABLE_CRASH_WATCHDOG=1` for debugger sessions that intentionally hard-kill the app.
- The main log is written through `startup::SharedLog`, a `Mutex<RotatingWriter>` held
  synchronously, **not** through `tracing_appender::non_blocking`. That wrapper cannot be
  used for a log a fatal failure depends on: `tracing-appender` 0.2 has no way to force its
  queue to disk (`NonBlocking::flush` is a no-op and `WorkerGuard` has no `flush`), and a
  failed startup ends the process moments later. Writing inline costs one uncontended mutex
  and one `write` syscall on the emitting thread, which is what the panic hooks and the
  diagnostics sampler already do, and it lets `Bootstrap::record_durably` `sync_all` the
  failure line before the error surface appears. A single shared handle also keeps rotation
  honest: two independent writers would each track their own byte count against the cap.
- `RotatingWriter::sync` exists for that one caller. `write_line` fsyncs every
  `SYNC_EVERY` lines, which suits a continuous sampler but not a record that must survive a
  process ending immediately after it.

### Startup bootstrap and fatal startup failures (issue #1525)

Diagnostics exist **before** the database does. `startup::bootstrap` runs first inside Tauri `setup` and does
three things in order: resolve the app-data profile, open the size-bounded `buildmesh.log`, and install the
tracing subscriber. Everything after it (`db::init`, preferences, the v19 repair migrations, harness
detection) is already inside a live `tracing` pipeline. Before this, all of that ran before any subscriber
existed, so its `warn!`/`error!` lines went nowhere: a user whose database would not open got no window, no
message, and no log, while the React Boot Error Panel (which needs the database to exist) nevertheless
promised that details had been written to `buildmesh.log`.

**One subscriber, installed once.** The fix is not a second, earlier subscriber, it is *this* subscriber,
installed as early as the profile directory allows and never replaced. So there is nothing to bridge and
nothing to rotate between two writers: early records are already in the same bounded `buildmesh.log` the rest
of the session appends to, and `try_init` is used rather than `init`, so a second attempt can never panic
with "a global default subscriber has already been set". The `tracing_subscriber::fmt().init()` that used to
sit in `setup` *after* `db::init` is gone; `Bootstrap::promote()` replaces it and only stops mirroring the log
to stderr. The writer is the same fixed-name `RotatingWriter` (`diagnostics::main_log_writer`) the rest of the
app uses, so a healthy startup still produces exactly one bounded log under the name the skills tail.

**Failures are typed, and retry is gated on stage.** `StartupFailure` carries a `StartupStage` (whose `label()`
is prose, because a modal dialog must not read `StartupStage::AppData`), a summary authored here rather than
derived from a driver error, a `SecretScrubber`-passed technical detail, and the resolved log and profile
paths. `StartupStage::is_retry_safe` is the behavioural line: `AppData` and `LogDirectory` fail before any
process-global state is installed, so re-running them is a real second attempt; everything from `Database`
onward is not, because `db::init` latches the global connection before it can fail and a second call
short-circuits to `Ok(())`. A "Retry" button on a database failure would therefore report success for a
database that is still broken.

**Corruption is reported, never repaired.** `startup::is_corruption` separates image damage (`DatabaseCorrupt`
/ `NotADatabase`) from reachability (`CannotOpen` / `PermissionDenied`), because the two send a user to
different places and a "your data is damaged" message aimed at a permissions problem would send them to
delete a perfectly good database. Nothing on this path renames, moves, or deletes a file: a corrupt database
is reported with its path and a `move ... .corrupt` command for the user to run themselves, and a test
asserts the file is byte-identical after classification.

**The error surface is native and pre-database.** `startup::present` shows a Win32 `MessageBoxW`, because a
Tauri command or the dialog plugin cannot help inside `setup` before the event loop starts (the plugin's
`blocking_show` deadlocks there, which is the same reason `instance_guard` raises a raw message box). The
button set is bound from the failure's action list by a pure, cross-platform-tested function, and the body
names what each Yes/No/Cancel does because Win32 will not relabel its buttons. The loop re-shows the dialog
until the user quits or a retry succeeds. `instance_guard::show_fatal_startup_error` is no longer used for
ownership failures: those now render through the same surface, so one failure reads like any other.

**Boundaries worth not re-litigating.** A `preferences.json` that will not parse is deliberately *not* a
fatal stage: the resolver accessors log and degrade to defaults by design, and refusing to launch over a
settings file would strand an app whose database is fine, so it is logged at `error` with the stage named
instead. Schema migration is not its own stage either, because `db::init` opens the connection and evolves
the schema in one call, so a failed migration *is* a `Database` failure by construction; only the
post-preferences v19 repair passes are separate, and those are non-fatal by design.

The frontend half is `commands::diagnostics::get_diagnostic_paths`, which hands `BootErrorPanel` the
resolved absolute locations from the bootstrap so the panel names a real path instead of a bare filename. It
performs no I/O of its own and cannot disagree with the files actually written.

## Environment Detection

- `env_for_path` — heuristics: `/mnt/`, `/home/`, `\\wsl$`, or `/` → WSL; everything else → Windows
- `to_host_path` — converts Linux paths to Windows UNC (`\\wsl$\Ubuntu\home\user`) for Windows-side file operations on WSL sessions

## Reproduction gotchas (Windows worktrees)

Preserve each file's encoding and line endings when editing; do not assume a repository-wide encoding. Prefer patch edits over shell whole-file rewrites. Inspect `git diff --check` and the actual diff for accidental encoding/whitespace churn. PowerShell double-quoted strings interpret backticks; use literal strings for Markdown/code snippets.

## Circuit architecture (spec #1205)
- **Capacity has separate run and agent leases (ADR-0028).** `meshes.circuit_run_capacity` admits pending Circuit Runs. Before admission, the worker reserves the blueprint's declared SpawnAgentNode footprint in `autopilot_circuit_run_agent_leases`; every spawn consumes that lease and remains bounded by the optional app-wide `circuit_agent_pool_size` when configured. Admission counts durable worst-case leases; a running Tick counts live circuit agents when applying that optional host-wide pool, so the two snapshots are intentionally not interchangeable. The legacy `meshes.autopilot_concurrency_limit` is not a circuit gate. Do not infer run authorization from mutable step agent ids.
- **The pending queue is durable and user-controlled.** `autopilot_circuit_runs.queue_position` is scoped per Mesh; worker admission and the Probe's complete queue both read it nearest-first. Moving a pending run swaps adjacent positions. Cancelling first commits the terminal `cancelled` state, then retires attached Agent Nodes; the worker rechecks durable state before every external effect. Cancellation and circuit deletion quiesce in-flight two-stage spawns under a worker barrier, and an aborted spawn retains its step ownership until process/worktree retirement succeeds. Circuit deletion disables triggers, applies the same cleanup contract to active and terminal runs (including retrying cleanup for already-cancelled runs), and removes the circuit ledger only after every retirement succeeds; failures retain the disabled ledger as the retry anchor.
- **Pure core / impure seam split:** `circuit/stepper.rs` computes `advance(run, event) -> Transition` without SQLite or network. `services/circuit_worker/mod.rs` owns the run loop, transition commits and effect application; it commits via `db::commit_circuit_advance` BEFORE executing external effects. `admission.rs` owns run admission and capacity snapshots, `turn_classify.rs` owns report interpretation and quiet-turn recovery, and `restart.rs` owns startup reconciliation and observer reattachment. `observation.rs` combines their observations into ordered events for the existing `RunView` seam. Its lazy live adapter preserves the order of native receipt reads, agent checks, job polling, approvals, capacity and waits; unit tests supply observations and time without SQLite, PTYs or classifiers. A literal event snapshot pins that ordering across extraction, while worker integration tests retain the real persistence checks. Admission reservations and running live-agent counts remain distinct snapshots of the same capacity policy. Circuit persistence is `db/circuit/{ledger,queue,leases}.rs` (issue #1660); `CircuitTriggerKind` lives on the graph model, not the IPC adapter.
- **Recovery uses the same transition invariants.** `set_step` owns inherited attempt changes and persists the fresh-attempt reset of outcome/error alongside the in-memory reset. Report readiness may override a stale Running display; durable completion checks session identity and input/report freshness without reinstating that display-status gate. Operator completion eligibility is shared by the evidence view and command and never fabricates native lifecycle evidence. OpenPr reconciliation uses a persisted per-attempt lookup allowance and never replays creation.
- **Effect replay policy:** spawn, PTY prompt, and GitHub effects claim a typed durable journal entry before external dispatch; an unknown result never replays automatically. Classifier continuation uses its pending/claimed context fence. `SetNodeStatus` commits with its completed step in one SQLite transaction; close effects retry idempotently while their association remains; notifications are transient. The complete per-effect and review-continuation inventory is `docs/development/circuit-effect-recovery.md`.
- **StepStatus vs StepOutcome are distinct.** `StepStatus` is the lifecycle column (queued / running / blocked / completed / failed / cancelled). `StepOutcome` routes edges (completed / failed / cancelled / blocked / working / green / red). A Blocked status parks a live step; a Blocked outcome is terminal classifier routing. `commit_circuit_advance` uses `StepOutcome::is_terminal_db_str` for terminal stamps.
- **RetryLimit semantics (#1207):** max_retries is the TOTAL allowed executions of the failing step, not retries beyond the first. With max_retries=3, attempts 1/2/3 all run; budget exhausted at attempt == max. Failed step is reset to Queued with attempt + 1, outcome/error cleared, started_at restamped via the fresh_attempt SQL path. The gate's own step is re-armed (Queued + fresh_attempt) when the same upstream fails again later so it can fire repeatedly across loops.
- **Reactive PTY wakeups (#1207):** `circuit::evaluator::on_output` wakes the circuit worker after each PTY chunk. `classify_step_turn` checks evaluator freshness and report binding before inference; validated native finished-turn evidence can establish readiness despite a stale Running display. Other recovery paths retain their evidence requirements. Freshness probes are scoped to the run, gate, and attempt, while durable context deduplicates the report that each gate consumed; a short cooldown retries a transcript that is published just after a yield. Failed classifications retry after 60 seconds even without new output, up to five failures per gate attempt. Durable exhaustion fences ordinary classification and quiet-turn inference across restart and fresh reports; only explicit evidence recheck resets the budget. The last sanitized backend error remains visible. The independent Circuit classifier selection accepts host-native configurations with an adapter-owned background inference recipe and saved model/effort; Codex runs ephemeral read-only inference outside the repository with shell and multi-agent tools disabled; an unavailable classifier is never routed as `WORKING`, and empty/suppressed reports are ignored. Parked gates retain an explanation in the step ledger. Every drive restores evaluator ownership for all attached agents, including completed spawn steps that still supply downstream classifiers. Transcript recovery uses an assistant-record revision (record position plus normalized assistant-text hash, not file mtime); prompt injection persists the preceding revision in the same transition commit before delivery so a permission yield or resume redraw cannot reuse it. Providers without recoverable transcript evidence require a live turn boundary before replayed output is trusted. See `docs/development/circuit-classifier-recovery.md`.
- **Review handoff and native completion:** initial and feedback gates require identity-bound lifecycle and report evidence. A Ready status cannot authorize initial review handoff. The Codex native adapter exposes explicit foreground turn completion; rollouts do not establish complete owned-work coverage. Recovery rejects newer transcript activity and atomically matches process incarnation, input ownership, last submission time and durable lifecycle stamp. Acquire SQLite before the per-agent input guard; global registry locks never span database work. Native Claude receipts are bound to a Buildmesh submission by content and ordering, not arrival: a `UserPromptSubmit` hook echoes the verbatim `prompt` and names the turn with `prompt_id` (Claude Code v2.1.196+), and Buildmesh matches that echo against the digest of the submission it recorded before writing to the PTY, while that submission is still the newest for the agent. A `Stop` inherits the binding by turn id. Receipts that cannot be bound this way - no `prompt_id`, no matching echo, a superseded submission, an ambiguous claim - keep reduced confidence. See `docs/learning/claude-code-harness-capabilities.md`.
- **Paused runs still occupy circuit-agent slots.** `count_running_circuit_steps` / `count_active_circuit_agent_nodes_total` include running and paused runs. The stepper stops scheduling while paused; lifecycle events may finish the current step, but scheduling resumes only after `Resumed`.
- **Zombie agents are reaped, never left running (issue #1793).** A self-throttled sweep on the worker tick (`services::circuit_worker::zombie_sweep`) transitions circuit-piloted nodes stuck `running` with no session identity and no readable report past 15 minutes to the terminal `Lost` status, then surfaces each transitioned node through `agent-lifecycle`. It follows the three-phase DB pattern — scan under a reader, transcript read and notification dispatch lock-free, batch write under the writer — and is scoped to nodes referenced by `autopilot_circuit_run_steps`, so a user's interactive node (e.g. a `terminal` shell with no captured identity) is never reaped. Per-agent observation is cooldown-limited. The circuit observer maps `Lost` to `AgentLost`, so an attached step cancels promptly instead of waiting out the first-observation window.
- **Circuit recovery is bounded and durable.** Missing session identities are retried through adapter-owned metadata discovery with a generation-checked conditional write. Wait clocks persist in run context per gate attempt: 15 minutes after yield, two hours without a new report while active, and a 15-minute first-observation fast-fail that ends a busy step whose agent has produced no session identity or report yet instead of holding the active budget, unless the step pins its own `timeout_seconds` (issue #1791); human approval gates do not expire; pause/resume suspends and renews the allowance. Classifier outages stop after five attempts. Only an explicit circuit CONTINUE verdict can prompt an owned agent, at most twice per attempt; pending delivery replays, while claimed delivery waits for evidence rather than risking duplicate input. Delivery rechecks the process/turn stamp and report revision. Failure/cancellation commits cleanup intent together with terminal state; a retry sweep protects borrowed/active references and retention preserves unfinished cleanup. See `docs/development/circuit-autonomy-audit.md` for evidence and limits.

### Saved Spawn Configurations

`preferences::spawn_configurations` owns named, capability-validated launch overrides scoped to one Spawn Option. Configurations live in application preferences; the backend menu includes each option's saved choices for mobile, while desktop management reads the same collection through IPC. The shared editor creates and edits configurations from Settings and spawn menus. Launch targets include unattached credentialed providers; saving a new route and recipe uses one preference transaction. Draft verification resolves the selected model without persisting the draft; verification records distinguish endpoint/model/runtime so checking one recipe does not replace another model's proof. Provider model metadata is independent of tier remaps, and allowed efforts intersect provider/model/surface metadata with harness capabilities. New-node creation commits the selected snapshot in `agent_nodes.spawn_configuration` in the same transaction as the node. Explicit per-call overrides win; omitted native fields retain the mesh/application/native cascade, while proxy models default to their route and do not inherit native harness model/effort defaults. A resolved proxy model reaches Codex as a single `--model`: `agent::spawn::command::build_spawn_command_prepared` folds the routing descriptor's model into the resolved config before `default_prepare` composes the recipe, so the adapter remains the single owner of the model flag and the orchestrator layer adds only `--profile` and the reasoning `-c` keys. The fold is load-bearing in both directions: the generated `<profile>.config.toml` carries only `model_provider`, so an empty cascade model would leave Codex on an OpenAI model against a foreign endpoint, and a second occurrence is rejected by the CLI as a repeated argument. Resume reads the snapshot, not the editable preference. A provider change cannot reuse another Spawn Option's snapshot.

## Mobile task navigation and idea capture

The mobile shell owns screen history and the selected home tab. NodeList owns
Overview/Work polling and attention events; NodeOverview polls the selected
node and sends short replies through the acknowledged HTTP input route.
CaptureIdea retains a browser-local draft until creation succeeds. Its prompt
crosses the generated CreateNodeRequest boundary into SpawnIntent::Prompt,
so the existing spawn orchestrator owns initial prompt delivery. The create
route validates text size and prompt capability before creating a node;
mobile never races terminal startup by injecting the idea as keystrokes.
