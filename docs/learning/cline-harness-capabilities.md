---
name: cline-harness-capabilities
description: Capability contract and Native Provider boundary for the Cline CLI harness, including the Buildmesh Model Provider env-var seam
metadata:
  type: reference
  harness: cline
  verified_cli_version: 3.0.62
  capture_resume_added: 2026-09-20
---

# Cline harness vs Buildmesh capability contract (2026-09-18)

Primary question: which Buildmesh harness capabilities does the Cline CLI
actually support, how is it spawned and resumed, and how do the user's
Buildmesh **Model Provider** accounts reach it without Buildmesh taking over
Cline's own authentication?

Verified against:

- Live `cline` 3.0.62 on Windows (npm shim at `%APPDATA%\npm\cline.cmd`),
  `cline --help` and `cline --version`
- Buildmesh adapter + trait: `src-tauri/src/agent/provider/adapters/cline.rs`,
  `provider/mod.rs`, `capabilities.rs`, `detection.rs`,
  `preferences/compatibility.rs`
- Related pages: [user guide](../user-guide.md),
  [troubleshooting](../troubleshooting.md)

## Executive answer

Cline is an interactive terminal agent that Buildmesh drives as a **Native
Provider**: Cline owns its own credentials (`cline auth`) and Buildmesh never
writes Cline's configuration. Buildmesh spawns `cline -i`, resumes with
`cline -i --id <id>`, and can forward a model, a reasoning effort, and a
prefill prompt. Attention is wired through Cline's file hooks (`TaskComplete`
→ turn completion, `SessionShutdown` → session exit); transcript reading is
**not** wired yet (issue #1776).

## What Buildmesh wants from a harness

| Flag / method | What Buildmesh uses it for | Cline |
|---|---|---|
| `supports_resume` | Enables the resume invocation and auto-resume on startup | Yes (`--id <id>`) |
| `auto_resume_on_startup` | Re-spawns suspended nodes that have a stored session id | Yes |
| `self_assigns_session_id` | Fresh spawn uses no mint flag; the id is captured later | Yes |
| `session_assign_args` | Fresh-spawn flags when Buildmesh mints the id | Empty (Cline self-assigns) |
| `resume_args` | Resume flags | `--id <id>` |
| `supports_model_override` | `--model <id>` from Mesh / app defaults | Yes |
| `effort_control` | Reasoning-effort vocabulary | `--thinking none\|low\|medium\|high\|xhigh` |
| `supports_prefill` | Seed the first turn from issue / handover text | Yes (`-i "<prompt>"`) |
| `supports_extra_args` | Verbatim circuit-author CLI flags | Yes |
| `requires_attention_hook` | Autopilot attention gate | Yes (file hooks; issue #1775) |
| `produces_readable_transcript` | Coordinator Node Digest / archive resume | No (reader not shipped) |
| `available_on` | Spawn Menu filtering | Windows, macOS, Linux |

## Cline's actual CLI

| Flag | Meaning | Buildmesh |
|---|---|---|
| `-i`, `--tui` | Open the interactive TUI | Baked into the base recipe |
| `<prompt>` | Positional prompt; seeds the first turn | Used as the prefill |
| `--id <session-id>` | Resume an existing session | Resume recipe |
| `-m`, `--model <model-id>` | Model for the session | Model override |
| `--thinking <level>` | `none\|low\|medium\|high\|xhigh` (bare flag = `medium`) | Effort override |
| `-P`, `--provider <id>` | Provider id (default `cline`) | Left at default |
| `-c`, `--cwd <path>` | Working directory | Provided by the spawn working directory |
| `--worktree`, `--kanban`, `-z`/`--zen`, `--team-name` | Cline's own orchestration surfaces | Never passed (duplicate Buildmesh's) |
| `--yolo` | Not present in 3.0.62 `--help` | Never passed |
| `--hooks-dir`, `CLINE_HOOKS_DIR` | Documented but inert in 3.0.62 | Never passed |
| `--data-dir` | Isolated local state; auto-enables Cline's sandbox | Never passed (`CLINE_DATA_DIR` stays a manual escape hatch) |

Session ids are self-assigned and shaped `<epochms>_<5 base36 chars>`
(for example `1789757012702_7of3e`). There is no flag that mints an id, so
Buildmesh cannot pin one at spawn. Live capture and suspended-node
recovery are handled by the SQLite pipeline documented in the next
section.

## Spawn recipe

- **Fresh:** `cline -i` — the TUI opens with no session id.
- **Resume:** `cline -i --id <id>` — the base `-i` survives composition.
- **With prefill:** `cline -i "<prompt>"`. Prefill normalisation is
  platform-aware: Windows flattens CR/LF/CRLF to single spaces (the
  `cmd.exe /c` end-of-command trap); macOS / Linux preserve the line
  structure and only normalise CRLF→LF (direct spawn, argv elements
  carry newlines safely). The platform-agnostic flattening that lived
  in the adapter for the first draft of this slice destroyed multi-line
  prompts on Unix; it now lives in
  `agent::launch::normalize_prefill_for_platform`.
- **Windows** wraps the spawn with `cmd.exe /c` (`WindowsShell::Cmd`),
  even when the resolved path is an absolute path off `PATH`.
  **macOS / Linux** spawn the executable directly
  (`WindowsShell::Direct`).
- **Off-PATH detection.** When Cline was detected via `CLINE_BIN_PATH`
  or the `node_modules\@cline\cli-windows-{x64,arm64}\bin` walk, the
  resolved absolute path is stored on the profile's `executable` field
  and threaded through `spawn_environment::wrap` as
  `executable_override`. `cmd.exe` resolves the npm shim directly via
  `cline.cmd` only when the npm prefix is on `PATH`; for everything
  else the orchestrator hands the absolute path to `cmd.exe /c`
  explicitly.

## Attention hook (issue #1775)

Cline's CLI resolves **file hooks** — executable files named exactly after an
event — additively from four fixed directories:

1. `~/Documents/Cline/Hooks`
2. `~/.cline/hooks` (`CLINE_DIR` overrides `~/.cline`)
3. `<workspace>/.clinerules/hooks`
4. `<workspace>/.cline/hooks`

All matching files for an event run together. The file's base name (case-
insensitive, with one of the extensions
`"" | .sh | .bash | .zsh | .js | .mjs | .cjs | .ts | .mts | .cts | .py | .ps1`
stripped) must equal the event name, so a file cannot be namespaced.
`--hooks-dir` / `CLINE_HOOKS_DIR` remain **inert** in 3.0.62.

Buildmesh provisions only the user-global root (`~/.cline/hooks`) with the two
events it can honestly normalise:

| File | `hookName` on stdin | Normalised kind |
|---|---|---|
| `TaskComplete.{sh,ps1}` | `agent_end` | `turn_completed` (node → Ready) |
| `SessionShutdown.{sh,ps1}` | `session_shutdown` | `session_exited` (node → Idle) |

Windows gets `.ps1` (`powershell -File`); macOS/Linux get `.sh` (`bash`). Both
extensions run without an exec bit. The script POSTs its stdin JSON to
`http://127.0.0.1:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID`
(the literal IPv4 loopback, so the callback never goes through DNS or the
machine proxy), expanding the port and node id from the environment Cline hands
the hook (`env: process.env`, no `env_clear`), so one node-agnostic file set
serves every node and no node id is ever baked in. The write is additive and
idempotent; a non-Buildmesh file at our exact path fails provisioning (surfaced
as `SignalHealth::Unavailable`) rather than being overwritten.

Cline auto-approves tools by default and Buildmesh passes no approval flag, so
**no permission or question signal exists** — `permission_requested`,
`question_requested`, `background_running`, and `process_idle` are deliberately
not advertised. Hooks are disabled in `--yolo` mode; the recipe never passes it.

## Native Provider boundary and the env-var seam

Cline runs as a **Native Provider**:

- Buildmesh **never** writes `~/.cline/data/settings/providers.json` and never
  invokes `cline auth`. Users add providers through Cline's own flow.
- Buildmesh injects the API key from a **Model Provider** account the user has
  already attached, at spawn time, as environment variables on the Cline
  process. No Buildmesh-side pairing is required for Cline itself: when no
  `cline`-specific pairing exists, Buildmesh falls back to any pairing for that
  account whose API surface Cline speaks (Anthropic or OpenAI). Attach the
  account once under Claude Code (Anthropic surface) or Codex (OpenAI surface)
  and `cline` inherits it.
- The canonical env-var set Cline reads for a given provider, and the full
  provider catalogue, are owned by Cline. Defer to `cline auth` for anything
  not listed here:

| Provider | Environment variable |
|---|---|
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| DeepSeek | `DEEPSEEK_API_KEY` |

> **Important consumer-aware branch (issue #1773 review).** The
> Anthropic surface emitter that targets **Claude Code** deliberately
> emits `ANTHROPIC_AUTH_TOKEN=<key>` and blanks `ANTHROPIC_API_KEY=""` for
> custom endpoints — that's the OpenRouter trap that forces Claude Code
> through the third-party token instead of a shell-set Anthropic key.
> Cline does **not** read `ANTHROPIC_AUTH_TOKEN`. If we naively fed
> that emitter to a Cline spawn, the Cline process would see
> `ANTHROPIC_API_KEY=""` and fail to authenticate. The env-builder
> therefore branches on the consumer harness: a `cline:<account>` spawn
> gets a Cline-shaped emitter (`ANTHROPIC_API_KEY=<key>` non-empty,
> `ANTHROPIC_BASE_URL=<base>` when set, `ANTHROPIC_MODEL=<primary>` when
> configured) — no `ANTHROPIC_AUTH_TOKEN`, no key-blanking. The
> OpenAI surface emitter is already the right shape for Cline so it
> is reused as-is.
>
> Regression-pinned by
> `preferences::compatibility::tests::cline_anthropic_default_endpoint_sets_anthropic_api_key`,
> `…_custom_endpoint_sets_anthropic_api_key_not_blank`, `…_openai_custom_endpoint_sets_openai_api_key`,
> and the Claude-Code contract pinned by
> `…_claude_anthropic_custom_endpoint_still_uses_auth_token_trap`.

## State and isolation

Cline uses one shared global `~/.cline`. There is no per-node `--data-dir`, so
the capture/resume invariant reduces to "same `--cwd`". `CLINE_DATA_DIR`
remains available as a manual per-Mesh escape hatch for a Mesh that explicitly
demands isolation, but Buildmesh does not set it. Be aware that concurrent
Cline processes share the same local session database and hub daemon; if you
run many Cline nodes at once, watch for lock contention and hub port reuse.

## Session-id capture and auto-resume (issue #1774)

Cline's TUI never prints its self-assigned session id live (it only
surfaces it in the end-of-run summary), so the PTY labeled-UUID regex in
`session_capture` cannot bind a fresh spawn. Buildmesh captures the id
from the authoritative local SQLite store at
`<cline home>/data/db/sessions.db`, then writes it to
`agent_nodes.cli_session_id` so auto-resume on the next startup can
splice `--id <id>` into the spawn argv.

### Fresh-spawn capture

- The adapter calls
  [`services::cline_session::start_capture_poller`](../../src-tauri/src/services/cline_session.rs)
  from
  [`AgentProvider::after_fresh_spawn`](../../src-tauri/src/agent/provider/mod.rs).
  The poller retries at 400 ms / 800 ms / 1.6 s / 2.5 s / 4 s (≈9.3 s
  total budget) until a row whose `session_id`'s embedded epoch ms is
  at or after `spawn - 2 s` appears in the SQLite store for the spawn
  `cwd` (round 1 review: Cline's `sessions.started_at` is an ISO 8601
  string, but the freshness gate operates on the epoch ms encoded in
  `session_id` directly — no ISO parsing needed).
- It picks the newest row that (a) matches the spawn directory under
  the platform-aware `env::directories_match` rules, (b) carries a valid
  Cline root id (`<epochms>_<5 base36>` legacy or
  `session_<epochms>_<5 or 6 base36>` current, subagent ids excluded), and
  (c) is tagged `interactive = 1` (one-shot prompt runs are excluded —
  they exit immediately per issue #1769).
- The id is persisted via `db::set_cli_session_id_if_missing`, so a
  later, more authoritative capture (the on-disk `sessions/<id>/`
  fallback, when added) cannot clobber it.
- The poller cancels if the node leaves the process registry (killed /
  crashed before the TUI flushed) — no zombie writes for a node the
  user has already abandoned.

### Suspended-node recovery (startup sweep)

The startup resume path (`services::session_recovery`) calls
`AgentProvider::recover_suspended_session_id`, which delegates to
`services::cline_session::find_historic_id_for_directory`. The helper
reads the same SQLite store without the spawn-anchor `not_before` floor
and applies `services::session_recovery::select_recovery_identity` —
the same one-candidate-in-window gate every other harness uses to avoid
binding the wrong conversation when a user reopens the same directory
twice. Two viable interactive rows in the spawn window → recovery
returns `None`, the node stays suspended, and the sweep retries on the
next startup.

### Override precedence

`env::cline_db_path_for_env` resolves the SQLite store from the spawn
environment in this order, mirroring `--help` (the issue #1769 source
of truth):

1. `CLINE_DATA_DIR` env var (if set and non-empty) — IS the data
   directory itself (per `cline --help`: `--data-dir` default is
   `~/.cline/data`), so the DB sits at `$CLINE_DATA_DIR/db/sessions.db`
   rather than `<home>/data/db/sessions.db`.
2. Otherwise the spawn environment's `~/.cline`, with the same WSL
   guest-home probe every other harness uses
   (`env::wsl_home()`).

A Windows-side Buildmesh driving a WSL Cline still reads the
guest-side store via `cline_db_path_for_host(EnvType::Wsl, …)`; the same
shape the Codex and AGY adapters use for cross-env capture.

### Limitations

- `~/.cline/data/db/sessions.db` is the **only** capture source today.
  The on-disk `~/.cline/data/sessions/<id>/` tree is a documented
  fallback for future work; the capture poller does not consult it.
- `--data-dir <path>` is honoured by the resolver, but Buildmesh never
  sets it — concurrent Buildmesh-spawned Cline processes share one
  store, and the row matchers use `cwd` to disambiguate. The
  `interactive = 1` filter excludes one-shot prompt runs that would
  otherwise pollute that shared view.

## Troubleshooting

- **Windows Application Control / antivirus blocks `cline.exe`.** Cline's own
  diagnostic asks you to run the binary path directly and check
  `Get-AuthenticodeSignature`. Buildmesh does **not** auto-unblock a blocked
  binary — resolve the block with your organisation's tooling, then restart
  Buildmesh so detection refreshes.
- **npm shim vs direct binary.** When the npm prefix (`%APPDATA%\npm`) is
  on `PATH`, Windows spawns through `cmd.exe /c cline`, which resolves
  the `cline.cmd` shim, which runs Node, which loads the platform
  binary. When Cline was detected via `CLINE_BIN_PATH` or the
  `node_modules\@cline\cli-windows-{x64,arm64}\bin` walk, the
  orchestrator hands the resolved absolute path to `cmd.exe /c` so the
  same `cmd.exe`-wrapped spawn shape is preserved. The CA-cert
  harvesting the npm shim does (`~/.cline/cli-node-extra-ca-certs.pem`
  → `NODE_EXTRA_CA_CERTS`) only fires on the npm-shim path; if you
  use the resolved-direct path you opt out of that wrapper. (Both
  architectures — `x64` and `arm64` — are probed because `@cline/cli`
  ships separate platform-specific packages.)
- **`CLINE_BIN_PATH`.** Set this environment variable to an absolute path to
  have detection prefer a specific Cline executable. The resolver walks
  `CLINE_BIN_PATH`, then the npm shim, then
  `node_modules\@cline\cli-windows-x64\bin`, then
  `node_modules\@cline\cli-windows-arm64\bin`. The first one that exists
  wins, and its absolute path is propagated to the spawn command
  through the profile's `executable` field.
- **WSL.** Cline in WSL is not a supported or tested runtime in this release.
  The guest-side and Windows-from-WSL detection probes deliberately skip
  Cline, so a WSL-only install does not produce a Spawn Menu row.
- **A Cline row does not appear.** Confirm `cline` (or `cline.cmd`) is on
  PATH, or that `~/.cline` exists, then restart Buildmesh. Detection runs once
  at startup.
- **`cli_session_id` stays `NULL` after a fresh spawn.** The poller
  retries for ~9.3 s before giving up. The two most common causes:
  - The Cline TUI was started without `-i` (a one-shot prompt run) —
    the SQLite row carries `interactive = 0` and is filtered out by
    design. Switch the Spawn Menu entry to **Cline (interactive)**.
  - The Cline process was killed before it flushed the `sessions`
    row. Check `~/.cline/data/db/sessions.db` with `sqlite3` to confirm
    whether a row was written for the spawn cwd at all.
- **Auto-resume splices the wrong session id.** Two interactive rows
  for the same directory inside the 5-minute spawn window will cause
  recovery to refuse to bind rather than guess. The node stays
  suspended; manually paste the desired id into the Spawn Menu or wait
  for the older row to age out of the window.

## Sources

- `cline --help` / `cline --version` (3.0.62)
- Buildmesh `agent::provider::adapters::cline` adapter and its unit tests
- Buildmesh `agent::detection` (install resolver order) and
  `preferences::compatibility` (`resolve_pairing` surface fallback)
