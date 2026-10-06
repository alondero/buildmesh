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
prefill prompt. Attention is wired through Cline's `TaskComplete` file hook,
which reports a **completed turn** as `turn_completed` (node → Ready), and the
transcript reader is wired (issue #1776), so the Node Digest carries real turn
content and an archived Cline node can be resumed from the picker. Cline's file
hooks expose no clean-exit or failure signal, so those are not claimed.

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
| `produces_readable_transcript` | Coordinator Node Digest / archive resume | Yes (reader wired; issue #1776) |
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
2. `~/.cline/hooks` — `resolveClineDir()` honours `CLINE_DIR` (read by Cline's code, though it is absent from the CLI's env table). Buildmesh resolves the same override, so the hook lands where Cline actually searches rather than in a directory it never reads.
3. `<workspace>/.clinerules/hooks`
4. `<workspace>/.cline/hooks`

All matching files for an event run together. The file's base name (case-
insensitive, with one of the extensions
`"" | .sh | .bash | .zsh | .js | .mjs | .cjs | .ts | .mts | .cts | .py | .ps1`
stripped) must equal the event name, so a file cannot be namespaced.
`--hooks-dir` / `CLINE_HOOKS_DIR` remain **inert** in 3.0.62.

Buildmesh provisions only the user-global root (`~/.cline/hooks`) with the one
event its file-hook layer can deliver on a clean run:

| File | `hookName` on stdin | Normalised kind | Fires when |
|---|---|---|---|
| `TaskComplete.{sh,ps1}` | `agent_end` | `turn_completed` (node → Ready) | `afterRun` with `status === "completed"` |

**Nothing else is claimed.** In Cline's file-hook layer
(`sdk/packages/core/src/hooks/hook-file-hooks.ts`) the only
`session_shutdown` dispatch is `runSessionShutdown`, reached from `afterRun`
only when `result.status === "aborted" || isAbortReason(result.error?.message)`
— i.e. on a user **abort/interrupt of a still-live session**, never on
teardown. So Cline's file hooks expose no clean-exit signal (a node's exit is
still observed through PTY EOF, as before) and no failure signal Buildmesh
provisions; `SessionShutdown`, `TaskError`, and the permission/question
surfaces are deliberately left unprovisioned and unadvertised. A stray
`session_shutdown` POST is classified lifecycle-neutral rather than as an exit.

Windows gets `.ps1` (`powershell -File`); macOS/Linux get `.sh` (`bash`). Both
extensions run without an exec bit. The script POSTs its stdin JSON to
`http://127.0.0.1:$BUILDMESH_PORT/api/attention/$BUILDMESH_SESSION_ID`
(the literal IPv4 loopback, so the callback never goes through DNS or the
machine proxy — the PowerShell path disables the default proxy and the POSIX
path passes `--noproxy '*'`), expanding the port and node id from the
environment Cline hands the hook (`env: process.env`, no `env_clear`), so one
node-agnostic file set serves every node and no node id is ever baked in.

The write is additive and idempotent; a non-Buildmesh file at our exact path
fails provisioning (surfaced as `SignalHealth::Unavailable`) rather than being
overwritten. The POSIX script requires `curl` on `PATH`; on a host without it
the callback is a silent no-op (the spawn still succeeds and only the attention
signal is lost) — the same dependency the other harness hooks carry.

Cline auto-approves tools by default and Buildmesh passes no approval flag, so
**no permission or question signal exists** — `permission_requested`,
`question_requested`, `background_running`, and `process_idle` are deliberately
not advertised. Buildmesh's recipe is `cline -i` (the interactive TUI) and never
passes `--yolo`, so the file-hook layer is active.

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

- `~/.cline/data/db/sessions.db` is the only **capture** source today. The
  on-disk `~/.cline/data/sessions/<id>/` tree is the **transcript** source
  (next section); the capture poller does not consult it.
- `--data-dir <path>` is honoured by the resolver, but Buildmesh never
  sets it — concurrent Buildmesh-spawned Cline processes share one
  store, and the row matchers use `cwd` to disambiguate. The
  `interactive = 1` filter excludes one-shot prompt runs that would
  otherwise pollute that shared view.

## Transcript reader (issue #1776)

`produces_readable_transcript` is `true`, which turns on two products: the
Coordinator **Node Digest** rich layer and the **archived-node resume picker**
(`resumable = supports_resume() && produces_readable_transcript()`). The reader
lives in `services::transcript_reader::adapters::cline` and resolves
`TranscriptFormat::Cline`.

### Where the data comes from

Cline keeps a per-session directory under
`<cline data dir>/sessions/<session-id>/` with two JSON documents:

| File | Carries | Read by Buildmesh |
|---|---|---|
| `<id>.json` | Session manifest — `status`, `ended_at`, `exit_code`, `prompt`, `metadata.title`, `metadata.git.branch`, `cwd`, `provider`, `model` | Not by the reader (every field is already on the node row) |
| `<id>.messages.json` | `{version: 1, messages: [...], system_prompt}` — the turns | **Yes**, the only turn source |

The data dir is `$CLINE_DATA_DIR` when set, otherwise `~/.cline/data` — the
same override the capture poller resolves, so the reader and the poller can
never disagree about which store Buildmesh is reading. The session id is
validated with `services::cline_session::is_cline_session_id` before it reaches
the path join; that validator's charset (`[0-9a-z_]`) is also what keeps a
corrupt `cli_session_id` from traversing out of the sessions root.

**Sources deliberately not used**, all settled by the #1776 research:

- `sessions.db` — stores **no message content** (only a `messages_path`
  pointer), and its `transcript_path` column is reserved and empty in every row.
- `cline history --json` — usable, but its `metadata` embeds the full
  `systemPrompt`; the file is a better source.
- `cline history export <id>` — emits **HTML**, not structured data.
- `session-search.db` — a derived FTS5 index, best-effort and possibly
  stale/unavailable. `tasks.db` is kanban, unrelated.
  `workspaces/<hash>/workspaceState.json` is near-empty in practice.

### Parsing rules

- **One JSON object, not JSONL — and rewritten wholesale.** Cline re-serialises
  the whole document with a non-atomic `writeFileSync` at each
  `iteration_end` (no append, no temp+rename), so the file is **not
  tail-able**: every read parses the whole document, and a read racing a write
  can catch it empty or truncated. The reader never assumes a complete
  document on a single read.
- **Gate on `version === 1`.** Cline's own Zod schema uses `$strip`, so the
  reader tolerates unknown *keys* for forward compatibility — but a different
  `version` is a different, unverified message shape.
- **The embedded `system_prompt` is skipped.** It duplicates the manifest's
  and dominates the file size; it must never surface as dialogue.
- **Turns are user-delimited.** Cline writes no turn index, and role transitions
  alone are not enough — a `role: "user"` message may be a harness notice. A
  user turn opens at each non-notice user message; the assistant messages up to
  the next one are that turn's work, coalesced into one turn (an assistant run
  is usually split across several messages: text, then a tool call, then the
  closing text).
- **Notices are filtered** by `metadata.kind` (`compaction`,
  `compaction_summary`, `auto_compaction`, `compaction_budget_emergency`,
  `completion_reminder`, `loop_detection_notice`, `mistake_stop_notice`,
  `recovery_notice`, `manual_compaction`), by `metadata.displayRole`
  (`system` / `status` / `error`), and by the presence of
  `metadata.userRunSpan`. This is also how a **compacted** session is accounted
  for: Cline re-materialises the collapsed prefix into `messages`, so without
  the filter a compaction summary would open a spurious "user turn" and swallow
  the real turns after it.
- **Prompts arrive wrapped** in `<user_input mode="act">…</user_input>`; the
  tag is stripped and the mode captured. A wrapper that is malformed or
  unexpected passes through unchanged rather than emptying the turn.
- **Blocks**: `text` becomes turn text; `tool_use` becomes a `ToolCall`;
  `thinking`, `redacted_thinking`, and `tool_result` are transport details and
  contribute neither.

### Degradation ladder

Never a crash, never a silent omission — the digest reports a typed reason:

| State | Result |
|---|---|
| No captured `cli_session_id` | `NoSession` |
| Session directory or `<id>.messages.json` absent | `NoTranscript` |
| Document truncated, not an object, `version ≠ 1`, or no `messages` array | `ShapeChanged` (loud — a busy node must never look quietly finished) |
| Well-formed but no dialogue yet (notices only) | `Empty` |

### Deliberate non-goals

- **Session telemetry is not surfaced.** `metadata.usage` /
  `metadata.aggregateUsage` (`inputTokens`, `outputTokens`, `cacheReadTokens`,
  `cacheWriteTokens`, `totalCost`, plus `aggregatedAgentsCost` for spawned
  agents) are **observed-session** totals, not account quota, and this reader
  does not present them anywhere. Every locally observed session during the
  research was failed or trivial with all-zero usage, so the non-zero shape is
  asserted from the type contract rather than observed — treat those numbers as
  unverified until someone runs a real multi-turn session. Keep them distinct
  from any account-level usage surface.
- **No circuit report adapter — a deliberate non-goal.** The Circuit report
  reader (`report_snapshot.rs`) is **line-oriented**: it requires a trailing
  newline, parses each line as a standalone JSON record, and reads the
  publication time from a per-record `timestamp`. A Cline transcript is a single
  JSON object, so `report_snapshot::read` refuses the format up front with
  `ReportReadError::Unsupported`. This is load-bearing, not cosmetic:
  `readiness::prepare` admits hook-native evidence for exactly
  `Unsupported | NoTranscript | Unreadable`, so a Cline circuit keeps running on
  its `agent_end` receipt. Had the document fallen through to the line reader it
  would report `PartialPublication` ("still publishing a transcript record") or
  `NoReport` ("has not published an assistant report") — both false for a
  complete document, and both in the set that *discards* the receipt, stalling
  the circuit behind a blocker it can never clear. So Cline's turn evidence is
  the hook receipt, and its digest is rich; it has no *report* read. For the
  same reason there is no native circuit turn boundary: `line_has_assistant_text`
  is a per-JSONL-line predicate with no meaning for a document, and the 256 KiB
  tail window that feeds `completed_turn` would truncate a document that has to
  be parsed whole.

## Circuit lifecycle and ownership (issue #1902)

Validated against **Cline 3.0.62** on Windows: the npm shim
(`%APPDATA%\npm\cline.cmd`) resolves `node_modules/cline/bin/cline`, which
selects the compiled `@cline/cli-windows-x64` binary (macOS/Linux spawn
directly). Because the npm package ships a compiled bundle rather than the
`.ts` sources, the facts below were read from the shipped SDK
**type declarations** (`@cline/core/dist/hooks/hook-file-config.d.ts`,
`hook-file-hooks.d.ts`) and the **compiled bundle**
(`@cline/core/dist/index.js`), not from `sdk/packages/core/src/…`.

No controlled live Circuit run has been performed for this provider, so
Circuit execution stays visibly **unsupported/Unverified**. The observer
policy records that explicitly (`services::circuit_worker::observer_policy`,
`cline` arm) rather than falling through to the generic fallback.

The file-hook config table maps exactly:

| Config file | `hookName` | Notes |
|---|---|---|
| `TaskStart` | `agent_start` | `beforeRun` |
| `TaskResume` | `agent_resume` | |
| `TaskCancel` | `agent_abort` | abort branch only |
| `TaskComplete` | `agent_end` | **the only event Buildmesh provisions** |
| `TaskError` | `agent_error` | not provisioned |
| `PreToolUse` | `tool_call` | not provisioned |
| `PostToolUse` | `tool_result` | not provisioned |
| `UserPromptSubmit` | `prompt_submit` | not provisioned |
| `PreCompact` | *(undefined)* | never serialised |
| `SessionShutdown` | `session_shutdown` | reached **only** from the abort branch of `afterRun` |

| Fact | Verdict |
|---|---|
| Supported version / platform | 3.0.62, Windows native (`cmd.exe /c` over the npm `.cmd`); macOS/Linux direct spawn unexercised for Circuits |
| Hook source | `agent_end`, dispatched from `afterRun` only when `result.status === "completed"` — a genuine completed-turn signal. The payload is `{clineVersion, timestamp, taskId, sessionContext, workspaceRoots, userId, agent_id, parent_agent_id, hookName, iteration, turn:{outputText, status}}`: it carries the **session** (`taskId`) and **no turn id, no prompt echo and no input stamp**, so no Buildmesh submission can be correlated |
| Clean exit | None. `SessionShutdown` → `session_shutdown` fires only when `result.status === "aborted"` or the error message reads as a cancel/abort/interrupt — i.e. on a user interrupt of a **still-live** session, never on teardown. A node's exit remains PTY-EOF evidence |
| Pull source | `<cline data dir>/sessions/<id>/<id>.messages.json` transcript only (`TranscriptFormat::Cline`); there is no native turn-completion pull (`completed_turn` has no meaning for a whole-document read) |
| Freshness / recheck bounds | Yielded 30 s via the observer policy; active budget unchanged. No adapter-owned recheck is wired — a missing hook parks Unverified until the watchdog budget expires |
| Foreground lifecycle | No validated adapter: `agent_end` proves a *turn* ended, never that the foreground process terminated |
| Child / background coverage | None. Cline has an internal subagent model (`agent_id` / `parent_agent_id`) and reconciles it on its own `session_shutdown`, but exposes no registry Buildmesh can pull, and Buildmesh never passes Cline's background surfaces (`--kanban`, `-z`/`--zen`, `--team-name`). Unknown child/background work never becomes completion (`WorkEvidence` requires `ownership_covered`, which no Cline source sets) |
| Final report | Assistant text from `<id>.messages.json` may inform interpretation (scrubbed, partial/unavailable labelled); it cannot prove lifecycle termination or ownership |

**Cline produces no native Circuit receipt at all.** `NativeHook::parse_value`
admits only `claude` / `claude_code` / `anthropic` / `codex` / `agy`, so a
Cline turn is visible evidence in run history and a durable attention receipt,
but it can never complete a step.

Cline is the riskiest cross-harness payload shape in the tree, because it keys
its event under **`hookName`** — the same key the Claude/Codex branch reads as
a fallback — while shipping `taskId` and `agent_id`, the exact fields the
generic parser folds into `session_id` and `child_id`. Malformed or
mislabelled Cline data therefore must not be able to borrow another harness's
lifecycle. Pinned by regression test
(`services::circuit_worker::native_hooks::tests::cline_hook_payloads_never_enter_the_native_hook_path`):
every 3.0.62 payload shape parses to `None` under **every** provider id,
including `anthropic`, `codex` and `agy`, while a sibling harness's own event
still parses under its own id.

Identity fencing for this provider is pinned in
`observer_policy::tests::cline_policy_advertises_no_authoritative_evidence`:
foreground turn receipts are reduced-confidence and deduplicate, a foreign
session id is rejected, a post-restart incarnation cannot carry over, an
unanswered request blocks verification, a cancelled turn stays unresolved, and
an unregistered child work id closing never verifies completion.

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
- Installed 3.0.62 hook typings: `@cline/core/dist/hooks/hook-file-config.d.ts`,
  `@cline/shared/dist/hooks/events.d.ts`
- Upstream hook implementation (the revision whose `hook-file-config.ts` the
  event-name table is taken from): `sdk/packages/core/src/hooks/hook-file-config.ts`,
  `sdk/packages/core/src/hooks/hook-file-hooks.ts` (the `afterRun` /
  `runSessionShutdown` dispatch that makes `session_shutdown` abort-only), and
  `sdk/packages/shared/src/storage/paths.ts` (`resolveHooksConfigSearchPaths`,
  `resolveClineDir`)
- Buildmesh `agent::provider::adapters::cline` adapter and its unit tests
- Buildmesh `services::transcript_reader::adapters::cline` — hook classifier
  and transcript reader
- Buildmesh `services::cline_session` (session store layout, id shape)
- Buildmesh `agent::detection` (install resolver order) and
  `preferences::compatibility` (`resolve_pairing` surface fallback)
