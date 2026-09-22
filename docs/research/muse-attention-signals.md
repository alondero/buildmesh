# Muse attention/turn signals (issue #1709)

Research date: 2026-09-12. Observed against **Muse Code 1.1.1 (1.1.1-R2514.1)**, build
`b934305d21`, installed at `/home/alond/.local/bin/muse` in WSL Ubuntu. All observations are
local and unpaid (no model calls were made to establish the surfaces below; the recorded
session logs are from pre-existing interactive runs).

Question: can Buildmesh obtain a **Node Turn** signal from the interactive Muse TUI, the
way it does from Claude/Codex/AGY/Grok hooks or the Command Code transcript watcher? Muse
declared `requires_attention_hook() == false` and no `attention_capability`, so Muse nodes
emitted no turn signal and Autopilot stayed blocked behind the compatibility gate.

## (a) MSP method index — `muse schema generate-json-schema`

`muse schema` describes itself as "export the MSP wire schema embedded in this binary …
the method index describes the served command plane (`session/*`, `turn/*`, `model/list`,
`view/*`, `approval/*`, `userInput/*`)". Running it:

```
$ muse schema generate-json-schema --out /tmp/muse-schema
wrote manifest.json, msp.schema.json to /tmp/muse-schema (stable surface)
$ cat /tmp/muse-schema/manifest.json
{"experimental":false,"fingerprint":"sha256:c669a30c…","schemaVersion":1}
```

The schema is precomputed at build time, so it is exact for this binary. It has five
top-level sections: `$defs`, `capabilities`, `description`, `errors`, `methods`,
`notifications`, `reserved`.

**Methods (client → host commands):**

| Group | Methods |
| --- | --- |
| `session/*` | `start`, `resume`, `read`, `list`, `fork`, `compact`, `setModel`, `setApprovalMode`, `userShell` |
| `turn/*` | `start`, `steer`, `cancel`, `interrupt`, `unqueue` |
| `approval/*` | `decide`, `listPending` |
| `userInput/*` | `answer`, `clarify`, `cancel` |
| `subagent/*` | `sendMessage`, `followupTask`, `interrupt`, `stop`, `resume`, `reopen`, `close`, `readResult` |
| `item/*`, `model/*`, `view/*` | `item/readOutput`; `model/list`; `view/page`, `view/subscribe`, `view/unsubscribe` |
| `initialize` | handshake |

**Notifications (host → client) — the lifecycle vocabulary:**

| Notification | Params | Meaning |
| --- | --- | --- |
| `turn/started` | `TurnStartedParams` (`sessionId`, `turnId`, `commandId`, `viewCursor`) | A foreground turn began running (fresh submits immediately; queued at launch; never steered). |
| `turn/completed` | `TurnCompletedParams` (`turnId`, `terminal`, `error?`, `reason?`, `usage?`, `durationMs?`) | Turn terminal: `completed \| failed \| cancelled`. `error` present iff `terminal == "failed"`. |
| `turn/retracted`, `turn/retryScheduled`, `turn/unqueued` | — | Interrupt/retry/queue bookkeeping. |
| `approval/requested`, `approval/resolved`, `approval/updated` | `Approval*Params` (`approvalId`, `itemId`, `turnId`, `availableChoices`, …) | Tool-approval open/decide/update. |
| `userInput/requested`, `userInput/settled` | `UserInput*Params` | Agent question open/settled. |
| `session/tokenUsage`, `session/contextUsage` | — | Already ingested for Observed Session Telemetry (#1680). |
| `session/{approvalModeChanged,modelChanged,goalChanged,todoListChanged,branchChanged,…}` | — | Session facts. |
| `item/{started,delta,updated,completed}`, `view/gap` | — | Transcript items and delivery-gap marker. |

Conclusion: the **agent loop has exactly the events attention needs** (`turn/*`,
`approval/*`, `userInput/*`). The open question was whether the interactive TUI path
exposes them — it does not; they are served on the MSP plane (see (c)).

## (b) Interactive TUI on-disk surfaces

`muse --help` has no hook/event flag. The TUI writes several artefacts under
`~/.local/share/muse`:

| Artefact | Shape | Usable as a turn signal? |
| --- | --- | --- |
| `sessions/YYYY/MM/DD/<uuid>/session.jsonl` | Append-only durable event log (NDJSON) | **Yes** — see below. |
| `sessions/.msp-view-v1/<uuid>/` | `HEAD.json` + binary `journal-*.bin` / `snapshot-*.json` | No — binary MSP view fold, not a text stream. |
| `tui-history.jsonl` | TUI input history | No — no turn boundaries. |
| `local-tracing/bootstrap/cli-<uuid>.log` | Human-readable tracing | No — debug spans, not a structured lifecycle stream. |
| `session-index.db` (`sessions` table) | Session metadata index (`session_id`, `session_log_path`, workspace, name, …) | Path resolver for the log above. |
| `sessions/**.peer-history.sqlite3` | Per-session peer history | No. |

The durable session log is the signal. Every run appends a `runtime.session` record whose
`payload.kind == "run"`:

```json
{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"<uuid>",
  "event":{"kind":"started","prompt":"…"}}}
{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"<uuid>",
  "event":{"kind":"terminal","terminal":"completed","reason":null,"turn_duration_ms":1027}}}
```

`event.kind == "terminal"` is the durable `turn/completed` fact; `terminal` is
`completed | failed | cancelled`. Task-level records (`payload.kind == "task"`) also carry
`completed`/`failed`/`cancelled` event kinds and must not be mistaken for run boundaries.

**Observed across 20 retained top-level session logs** (subagent logs excluded):

- `run/started` = **35**, `run/terminal` = **35** — exactly one terminal per run.
- Terminal values: `completed` 32, `cancelled` 1, `failed` 2.
- `approval/requested` records exist, but **only** in sessions whose
  `security_mode == "normal"`. Sessions launched with `--disable-approval`
  (`security_mode == "approval_disabled"`, `source: "explicit-cli-flag"`) carried
  **zero** `approval/requested` records.

The log is written live: in one retained session, records span the full ~11-minute run.

## (c) Could `muse serve` back attention for PTY nodes?

`muse serve` is "Serve an MSP session host over stdio": "The client owns this process's
stdin and stdout and is its only connection. Sandbox posture and session durability are
constructed here … Approval mode is … selected on the wire." It is a **headless JSON-RPC
host** — it does not render the interactive TUI. Adopting it would replace the PTY+TUI
execution model with a Buildmesh-owned MSP client and renderer. That is the separate
serve-path architecture, not a drop-in attention source for the existing PTY node. This
research deliberately does **not** duplicate it (see out-of-scope in #1709; MSP transport
is the #1681 surface).

## (d) Would untrusted-workspace hook files load?

> **Superseded for Muse 1.3.0 — see the update at the end of this document.** Hook files
> *do* exist on the version Buildmesh supports today (a plugin bundle); the finding below
> is the 1.1.1 baseline it was probed against.

Moot on 1.1.1: **no hook files exist**. `muse --help` exposes no hook registration; a recursive
search of `~/.config/muse`, the feature-config cache, and the runtime directory found no
hook configuration, and `~/.config/muse` holds only `auth.json`, `trust.json`, and
`settings.json`. Muse's `--trust-workspace` loads a workspace's *skills and rules*, not
hooks. There is therefore no trust prerequisite to satisfy and no trust issue to inherit
for an attention mechanism.

## Verdict and wiring

**Passive watcher** (option 2). A native hook is impossible (no registration surface), and
the MSP serve plane is a different architecture. The interactive TUI's durable session log
already carries the turn boundary, so `services::muse_watcher` tails it (mirroring
`commandcode_watcher`) and publishes a Node Turn on each `run/terminal` record. Muse keeps
`requires_attention_hook == false` and `attention_capability == None`; it now sets
`supports_passive_turn_watcher == true`.

**Launch mode is `SkipPermissions`.** Under Buildmesh's `--disable-approval`,
`PermissionRequested` is impossible by construction (zero `approval/requested` records in
every `approval_disabled` session), so the watcher classifies only turn terminals — never a
permission. `failed`/`cancelled` terminals still yield the node back to the user and are
published as turns with the terminal value carried in `completion_reason`.

The watcher resolves the log through the same `session-index.db` the adapter already uses
for session recovery, converting the guest path with `env::to_host_path` for WSL hosts. It
defers unterminated trailing lines (Muse may be mid-write), suppresses an earlier terminal
when a newer run has already started, and baselines resumed sessions at their pre-spawn EOF
so a prior completed turn is never replayed as the resumed node's signal.

## Update — Muse 1.3.0 (2026-09-21)

Re-probed against the version Buildmesh supports today: **Muse Code 1.3.0
(1.3.0-R3401.1)**, the native Windows build at
`%LOCALAPPDATA%\Programs\muse\muse.exe`. All observations are local and unpaid
(`--provider echo`, no model calls). This supersedes (d) and refreshes (a) and
(b); (c) is unchanged.

### (a) refreshed

`muse schema generate-json-schema` still exports the precomputed bundle; the manifest
fingerprint is now `sha256:7469c9e3…` (was `sha256:c669a30c…`). `turn/*`, `approval/*` and
`userInput/*` are unchanged. New since 1.1.1: methods
`goal/{set,edit,clear,pause,resume}`, `session/rename`, `session/setReasoningEffort`,
`skill/list`, `task/{background,stop,stopAll}`, `usage/read`,
`workflow/{cancel,childControl}`; notifications `session/statusChanged`,
`session/nameChanged`, `session/modelRouteUnserved`, `session/listChanged`,
`session/viewHealthChanged`, `usage/changed`, `skill/changed`. None of these change the turn
signal.

### (b) re-verified — the wired signal still holds

The durable-log contract the watcher depends on is unchanged on 1.3.0: `runtime.session`
records with `payload.kind == "run"` and `event.kind == "terminal"` (carrying
`event.terminal` = `completed | failed | cancelled`) are still appended. Across the 27
retained top-level 1.3.0 session logs: **58 `started` / 57 `terminal`** (the shortfall is one
node killed mid-run, not a format change). The record shape is byte-identical to the 1.1.1
sample in (b), so `services::muse_watcher` parses live 1.3.0 logs without change. See
[Harness attention reliability audit](../learning/harness-attention-reliability.md).

### (d) rewritten — a native hook surface *does* exist on 1.3.0

1.3.0 ships a **plugin system with a claude-compatible hook surface** (the 1.1.1 finding that
"no hook files exist" no longer holds). A bundle's manifest lives at a root `plugin.json`
with the exact Agent Plugins 1.0.0 `$schema`, or at exactly one nested `.muse-plugin/`,
`.codex-plugin/` or `.claude-plugin/plugin.json`; `muse plugins validate <dir> --json`
reports `manifest_family: "claude-compatible"` for the last.

Events accepted as `classification: "supported"`: `PreToolUse`, `PostToolUse`,
`PostToolUseFailure`, `Notification`, `Stop`, `SubagentStop`, `UserPromptSubmit`,
`SessionStart`, `SessionEnd`, `PreCompact`, `PermissionRequest`. Rejected as
`unsupported-hook-event`: `TaskCompleted`, `ApprovalRequested`, `UserInputRequested`,
`TurnStarted`, `TurnCompleted`, `MessageDisplay`, `BeforeTool`, `AfterTool`,
`PreToolUseFailure`.

The handler shape differs from mcode's: `{ "type": "command", "command": "<one shell
string>" }` — a separate `args` array is **rejected** (`unsupported-field`), and hooks must
be async observation-only.

**A `Stop` hook fires in a live session** (offline `muse exec --provider echo`) and delivers
a payload the existing attention route already classifies:

```json
{"hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"…",
 "session_id":"<uuid>","turn_id":"<uuid>","cwd":"…","transcript_path":null,
 "model":"unknown","permission_mode":"default"}
```

That is the Claude `Stop` shape `http::routes::attention` already maps to a clean turn
completion, so no new classifier would be needed.

**Activation is gated, which is why it is not wired.** `muse plugins install <path> --scope
user|project` copies the bundle into a **global** content-addressed cache
(`~/.local/share/muse/plugins/cache/local/<id>/<sha>/package`) plus a global `installed.json`
lockfile. A third-party bundle's hooks then sit at `status: "review_needed"` with
`effective_capabilities: []` ("third-party plugin: hooks require review before activation")
until an explicit, non-interactive `muse plugins approve <id>`. The **project-scoped** path —
node-local, no global state — is refused outright: *"project-local plugin source is blocked
because the workspace is untrusted"*, where trust is a per-workspace entry in
`~/.config/muse/trust.json` (issue **#1706**). Muse does not pass `BUILDMESH_*` to hooks, so
the callback URL must be baked per node — which changes the capability definition hash every
spawn, forcing a re-approve and a new cache directory per node.

### Verdict after the update

The **passive watcher remains the wired signal** and is unaffected by 1.3.0. The native hook
is now known to be *possible*, but wiring it today would have Buildmesh silently approve
arbitrary-command hooks into the user's global Muse state on every spawn, with per-node cache
growth — a consent and shared-state cost it should not pay unattended. **Wiring the hook
belongs in a separate follow-up ticket once Muse workspace trust (#1706) lands**, at which
point a project-scoped install avoids both the global cache and the consent bypass. This
update corrects the record; it does not change the wiring.
