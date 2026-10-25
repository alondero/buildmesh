---
name: mcode-harness-capabilities
description: MiniMax Code (mcode) capability review against Buildmesh's harness contract — transcript reader evidence and the validated attention hook
metadata:
  type: reference
  harness: mcode
  mcode_version: 0.6.13 (installed native Windows CLI and TUI)
  date: 2026-10-25
  attention_validation: SessionStart, UserPromptSubmit and Stop captured from
    installed 0.6.13 exec and interactive ConPTY with an isolated local BYOK endpoint;
    Buildmesh HTTP delivery and WSL remain unvalidated by this probe
---

# MiniMax Code harness capabilities vs Buildmesh

Review of what the `mcode` binary actually exposes, versus what Buildmesh's
Mcode adapter advertises and uses. Both primary concerns have landed:
**transcript understanding** (`TranscriptFormat::Mcode`) and **attention
hooks** (the provisioned compatible plugin). Native 0.6.13 evidence below
extends the original 0.4.12 completion validation. The Circuit compatibility
gate is open for mcode.

## Sources (primary only)

| Source | What it is |
|---|---|
| `@minimax-ai/code@0.4.12` npm bundle (`cli.js` + `chunks/*.js` strings) | Shipped CLI: data-dir resolution, session layout, plugin scan, hook payload |
| Installed `mcode 0.4.12` driven on Windows, 2026-09-20 | Live `exec` and interactive-TUI runs against a local listener (issue #1797) |
| Installed `mcode 0.6.5` on Windows, 2026-10-09 | `mcode --version` and interactive `mcode --help`: `-m, --model <provider/model>` selects the model for this Session only |
| [Official npm package metadata](https://registry.npmjs.org/@minimax-ai/code) and [0.6.13 package](https://registry.npmjs.org/@minimax-ai/code/-/code-0.6.13.tgz) | Latest stable npm version, publication timestamp, shipped changelog and hook implementation |
| Installed `mcode 0.6.13`, 2026-10-25 | Help and bundle inspection; native `exec` and interactive ConPTY against an isolated local OpenAI-compatible endpoint; no real model/account used |
| [Public release v0.6.5](https://github.com/MiniMax-AI/minimax-code/releases/tag/v0.6.5) | Separate source-release channel; does not establish the npm 0.6.13 implementation |
| [Public hook contracts](https://github.com/MiniMax-AI/minimax-code/blob/da22f11c55d4748155d7162c1030414228a00565/packages/agent-modules/plugin-hooks/src/contracts.ts), [hook runner](https://github.com/MiniMax-AI/minimax-code/blob/da22f11c55d4748155d7162c1030414228a00565/packages/agent-modules/plugin-hooks/src/runner.ts) and [TUI notice docs](https://github.com/MiniMax-AI/minimax-code/blob/da22f11c55d4748155d7162c1030414228a00565/docs/hooks.md) | Pinned public source comparison; shipped npm code and native captures take precedence where the channels differ |
| [Official BYOK examples](https://github.com/MiniMax-AI/minimax-code/blob/da22f11c55d4748155d7162c1030414228a00565/docs/examples.md) | Provider ids from `provider list`, model reference selection, supported API formats and local endpoint setup |
| [MiniMax CLI features](https://agent.minimax.io/docs/cli/features#model-references) | Model reference syntax: `provider/model`, optionally `#variant` |
| `MiniMax-AI/minimax-code-plugins` (`proposals/hooks-v0.4-spec.md`, `docs/plugin-compatibility.md`) | The mcode 0.4.0+ plugin format: `.claude-plugin/plugin.json`, inline `hooks` |
| `MiniMax-AI/minimax-code-plugins` (`proposals/hooks-detailed-spec.md`, `examples/hello-mcode-hooks`) | The superseded v0.3.x Agent-Plugin format (`io.minimax.mcode/hooks/hooks.json`) |
| `src-tauri/src/agent/provider/adapters/mcode.rs` | Current Buildmesh adapter |
| `src-tauri/src/services/transcript_reader/adapters/mcode.rs` | Current Buildmesh reader |
| `src-tauri/src/agent/capabilities.rs`, `circuit/compatibility.rs` | Harness contract and Circuit gate |

Delivery, layout, and payload claims below are backed by the live run; the
scanner and manifest rules are traceable to the shipped bundle and the
first-party plugin spec.

## Review against the latest npm release

Audience: Buildmesh integration maintainers. Status: current evidence as of
2026-10-25. The latest npm release is **0.6.13**, published 2026-10-23; the
latest GitHub source archive is **v0.6.5**, published 2026-10-09. Their matching
version labels do not imply matching artifacts: the installed npm changelog
contains releases and capabilities absent from the public source changelog.

The concrete attention gap is **`UserPromptSubmit`**. Buildmesh already
provisions identity capture and completion, but without the submit callback it
misses authoritative evidence that the next user turn began. Provisioning that
event and projecting it as work resumed lets the shared lifecycle clear old
attention and fence delayed completion callbacks by their prompt identity.

The second gap is the BYOK provider reference: `mcode provider list --json`
returns ids such as `custom_provider:probe`, and the supported override is
`custom_provider:probe/probe`. The old model validator rejected its colon.
Buildmesh now admits that exact provider namespace while retaining its safe
identifier grammar for the remaining provider, model and variant components.
The local native probe successfully selected that reference in exec and TUI.

The shipped 0.6.13 event list is `SessionStart`, `SessionEnd`,
`UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`,
`SubagentStart`, `SubagentStop`, `Stop`, `PreCompact`, and `PostCompact`.
`Notification`, `PermissionDenied`, `StopFailure`, and `StopCancelled` are
absent from that parser. The older twelve-event list has been corrected
to this released implementation. Failed or cancelled turns do not fire the
normal `Stop`; `SessionEnd` is best-effort session teardown, so it cannot stand
in for a reliable turn failure or cancellation event.

Other additions work inside the existing terminal without a Buildmesh adapter
change: hook `systemMessage` notices (0.5.8), system-prompt replacement and
append flags, lightweight conversations, Computer Use selection (0.6.13), and
the remote App Server (0.6.12). The TUI still has no permission or effort flag;
`exec --effort` and `exec --permission` do not extend interactive launch support.
Headless JSON/stream JSON, output schemas, and final-message files remain
separate entry-point capabilities. Lightweight mode omits coding tools and
cannot be combined with `--session` or `--continue`, so it is not an automatic
replacement for ordinary coding sessions.

Newer user/project `hooks.json` management is present in the shipped bundle,
but the existing Claude-compatible plugin remains loadable and needs no format
migration. Native `ask_user` questions can return while waiting for input;
unconditionally treating `PostToolUse` as work resumed would erase that wait.
Question hooks, noninteractive permission modes, remote App Server integration,
and cancellation detection need their own behavioral evidence before expansion.

### Native 0.6.13 delivery evidence

An isolated `MINIMAX_DATA_DIR` contained a plugin at the same
`plugins/io.buildmesh.attention/.claude-plugin/plugin.json` path and with the
same `command`/`args` registration form as Buildmesh. The command captured its
stdin. A local HTTP server returned a deterministic OpenAI-compatible streamed
completion; no signed-in MiniMax account or paid model request was involved.
Both `mcode exec` and the actual interactive TUI driven through Windows ConPTY
produced exactly this sequence:

```json
{"hook_event_name":"SessionStart","session_id":"mvs_example","prompt_id":"turn_example","source":"startup","cwd":"<workspace>","transcript_path":"<temporary compatible transcript>","model":"probe"}
{"hook_event_name":"UserPromptSubmit","session_id":"mvs_example","prompt_id":"turn_example","prompt":"Please say probe ok.","cwd":"<workspace>","transcript_path":"<temporary compatible transcript>","permission_mode":"bypassPermissions"}
{"hook_event_name":"Stop","session_id":"mvs_example","prompt_id":"turn_example","stop_hook_active":false,"last_assistant_message":"probe ok","cwd":"<workspace>","transcript_path":"<temporary compatible transcript>","permission_mode":"bypassPermissions","effort":{"level":"medium"}}
```

The example is sanitized; the real ids used the normal `mvs_<hex>` and
`turn_<suffix>` shapes. The exec used `--permission full`; the isolated TUI
kept its default `auto`, proving the same event sequence in both modes.
`SessionStart` is admitted with the first turn rather than guaranteed merely
by opening an empty TUI. `UserPromptSubmit` and `Stop` share **`prompt_id`**,
and the Claude-compatible projection omits `turn_id`; regression fixtures must
exercise the actual field. The runtime uses its native turn id as that prompt
id. `BUILDMESH_*` was absent in every captured hook environment, preserving
the need for baked callback targets. Explicit `args` still selects exec form
with no shell interpretation.

This validates native event delivery into the registered command. It does not
prove the entire Buildmesh HTTP route, a real model/account, macOS/Linux/WSL,
permission prompts, session teardown, or failure/cancellation delivery. The
minimum previously validated hook version remains 0.4.12; 0.6.13 delivery
evidence does not establish which earlier release first delivered submit hooks.

## What Buildmesh wants from a harness

Same checklist as the Grok review (`grok-harness-capabilities.md`): resume,
prefill, model/effort overrides, attention hook, readable transcript,
interactive TUI over PTY, no harness-owned worktree flag.

## What the Mcode adapter advertises

| Flag | Adapter value | Notes |
|---|---|---|
| `supports_resume` | `true` | `resume_args` → `--session <id>` |
| `auto_resume_on_startup` | `true` | |
| `self_assigns_session_id` | `true` | A delivered callback binds session id plus workspace. `SessionStart`, submit and completion were captured from native 0.6.13; first-turn admission owns startup identity. |
| `supports_prefill` | `true` | Trailing positional `[prompt]`, no `--prefill` flag |
| `supports_model_override` | `true` | Interactive `mcode 0.6.5` accepts session-only `--model <provider/model>`; supersedes the older limitation in issue #1179 |
| `effort_control` | `None` | The TUI has no supported effort override |
| `requires_attention_hook` | `true` | `Stop` delivered from a live 0.4.12 TUI (#1797); `SkipPermissions`, `TurnCompleted` only — see below |
| `attention_capability` | `Hook { events: [turn_completed], launch_mode: skip_permissions, min_version: "0.4.12" }` | Buildmesh launches mcode with an auto-approving policy, so no permission signal is claimed |
| `produces_readable_transcript` | `true` | Canonical `messages.jsonl` via `TranscriptFormat::Mcode` (this change) |
| Shell | `WindowsShell::Cmd` on Windows, `Direct` elsewhere | Correct — `.cmd` shim on Windows, native binary on macOS/Linux |
| Launch mode | Interactive TUI | Correct — PTY backend supports full-screen rendering |

## Session model selection

The native Windows `mcode.cmd 0.6.5` interactive help advertises `--model`
alongside `--session` and `--continue`, so model selection does not require
changing to `mcode exec`.
Buildmesh validates the `provider/model[#variant]` syntax, including the exact
BYOK namespace `custom_provider:<name>/model[#variant]`. Provider names, model
ids and variants accept only ASCII letters, digits, `.`, `_` and `-` to prevent
Windows Cmd parsing or expansion. Validation is adapter-owned and runs on configuration/default saves
and before spawn, including Circuit overrides. Accepted references are forwarded
unchanged on fresh and resumed launches, before the trailing positional prompt. Saved configurations
override native defaults; an absent model retains the existing default cascade.
This option applies to the session without changing mcode's global model default.
macOS, Linux and WSL remain unverified. No runtime model-flag version gate is
added: the accepted compatibility risk is that older installations lacking the
interactive option reject launches with a model inside the terminal. Update the
exact executable Buildmesh launches or clear the configuration and harness-default
models; see [troubleshooting](../troubleshooting.md#minimax-code-rejects-a-configured-model).
This help-level check does not validate inference for every account/model
combination or extend the separate attention evidence.

## Transcript — wired (this change)

### On-disk layout (bundle-verified)

Data dir resolution (`data-dir` chunk): `$MINIMAX_DATA_DIR` →
`$MAVIS_DATA_DIR` → `~/.minimax` (plus `-<profile>` suffix when a profile is
selected). The `~/.minimax-code` install directory is separate and never
holds sessions.

```
<dataDir>/v2/
  sessions/<YYYY>/<MM>/<DD>/<HH-MM-SS-mmm>-session_<base64url(sessionId)>/
    manifest.json   # v1: sessionId, createdAtMs, updatedAtMs, source, layout, paths
    messages.jsonl  # canonical history: one record per line
    snapshots/      # generation snapshots
    reports/        # report artifacts
    ledger.jsonl / display.jsonl
  sqlite/           # runtime-state.sqlite, session-index.sqlite, usage-state.sqlite
```

The directory name never carries the raw session id, so the locator walks
`v2/sessions/` newest-first and returns the `messages.jsonl` beside the
first `manifest.json` whose `sessionId` matches (mirroring the CLI's own
startup scan). Session directories are never descended into (`snapshots/`
and `reports/` are artifacts, not sessions), so per-poll cost stays
proportional to directory breadth, not all of history.

Data-dir caveat: under WSL the spawn-aware lookup resolves the guest
`$HOME/.minimax` and ignores a host-side `$MINIMAX_DATA_DIR` override —
same convention as the other harness home helpers.

### Record shape (bundle-verified)

```json
{"message_id": "msg-…", "turn_id": "…", "message": {
  "role": "user | assistant | toolResult | compactionSummary",
  "timestamp": 1788000000000,
  "content": [{"type": "text", "text": "…"}]
}}
```

Assistant content items: `{type: "text", text}`, `{type: "thinking",
thinking}`, `{type: "toolCall", id, name, arguments}` (arguments natively an
object, occasionally JSON text), `{type: "image", …}`. The reader maps user /
assistant records to `Turn`s, extracts `toolCall` items to `ToolCall`s under
the shared truncation caps, and skips `toolResult` echoes and
`compactionSummary` bookkeeping the way the Grok adapter drops `tool` /
`system` lines. Malformed records (missing `message`/`role`/`content`) flag
`ShapeChanged`; non-record lines skip quietly.

This flip surfaces mcode in the archived-node resume picker (`resumable =
supports_resume && produces_readable_transcript`), hydrates the Coordinator
Node Digest rich layer, and feeds circuit assistant reports.

## Background tasks — the CLI never wakes an idle session (issue #2105)

A `custom` record carries the CLI's task-completion reminder. It is not a turn,
so the reader above skips it:

```json
{"message_id": "msg-…", "turn_id": "…", "message": {
  "role": "custom",
  "customType": "background_task_cadence_reminder",
  "display": false,
  "details": {
    "version": 1,
    "tasks": [{"taskId": "bg_…", "status": "failed", "endedAtMs": 1791205267253}],
    "undeliveredTotal": 1, "queuedTotal": 0, "terminalTotal": 39,
    "cadence": {"assistantIterationsBeforeReminder": 0}
  },
  "timestamp": 1791216756792
}}
```

**The gap.** The reminder is injected at the *start of the next turn*, never
when the task ends. Measured across every reminder in recent sessions, each one
is preceded by a `user` record — the reminder follows a wake, it never causes
one. An idle session therefore takes no turn, receives no reminder, never reads
the finished task, and never reports it. Run 335 stalled 3 h 14 m on exactly
this: the task ended at `…267253`, the last assistant message was at `…156271`
(111 s earlier), and the next record of any kind was 3 h 14 m later.

`undeliveredTotal` is not a usable trigger either: it counts messages the CLI
has *not yet pushed*, so it keeps its last value for the whole idle window —
precisely the state that needs detecting.

**Why the reminder cannot drive the wake-up.** A detector built on the reminder
never fires during the stall it exists to break — the record it needs does not
exist until something else has already woken the session. So the detector is
built on two records that *do* exist while the session is idle:

1. **Which task this session launched** — the `bash_background`
   acknowledgement, written during the turn that starts the task:

   ```json
   {"role": "toolResult", "toolName": "bash",
    "content": [{"type": "text", "text": "<bash_background task_id=\"bg_…\">…"}],
    "details": {"status": "started", "task_id": "bg_…", "description": "…"},
    "timestamp": 1791205002057}
   ```

2. **When it ended** — the CLI's on-disk task store,
   `<dataDir>/background-tasks/<taskId>/`. `output.log` grows while the task
   runs and stops when it ends, so its modification time is the CLI's own record
   of the task's end: measured to **0 s** against the `endedAtMs` the reminder
   carries for the same task. `summary.txt` appears only once a task has
   finished and is the stronger signal where present.

`readers::mcode::detect_background_task_stall` combines them with the
transcript's last assistant message and reports a task that is over **and**
newer than it. That ordering is what separates "the agent is working on it"
from "the agent will never learn of it"; terminal silence alone is never
evidence. A task whose output has been written within `TASK_QUIESCENCE_MS` is
treated as still running, so a live task cannot consume the single wake-up an
attempt owns.

**Three terminal channels, all read.** A launched task counts as over when any of
these records it, because each misses a case the others cover:

- `summary.txt` in the task store — observed only for **succeeded** tasks (the
  failed and cancelled tasks examined in run 478's session left just `output.log`).
- A `toolResult` receipt from `task_output` with status `succeeded`, `failed`
  or `canceled`. A poll that races the task's exit can return the final output
  yet still say `running`, so this can be missing.
- The host's `<background-task-finished>` notice: a `user` record whose
  `hostMetadata.backgroundTaskOrigin.kind` is `background-task-terminal`, with
  `taskIds` and a `<task task_id="…" status="…" ended_at_ms="…"/>` element per
  task. Only the structured metadata counts; the notice text alone could be
  forged by a quote. For a failed task whose poll raced the exit this can be the
  only record (circuit run 478: a task exited 101 and no other record of it was
  found; the stale "pending" count made the final `Stop` read as background
  work, so the node stayed `running`). A notice settles the task at its
  `ended_at_ms`, else the record's timestamp; if neither is usable the task is
  still settled but undated, and the stall detector does not report an undated
  task (it cannot be ordered against the assistant's last message). An undated
  notice never erases a date an earlier receipt or summary already gave the task.

The worker turns that into `CircuitEvent::NudgeIdleSession`, which the stepper
turns into at most one `Effect::NudgeIdleAgent` per step attempt, fenced to the
observed turn's `(report revision, turn stamp)`. Delivery reuses the
continuation input-ownership guard and is recorded through
`record_prompt_submission`, so the wake-up appears in `circuit_run_history` as
an ordinary `prompt_submitted` row.

Known limits, stated rather than papered over:

- One wake per step attempt. A second stall inside the same attempt is left to
  the operator rather than nudged again.
- The quiescence window is a fixed 3 minutes. A task that flushes output less
  often than that could be nudged while still running; the agent then reports it
  as still running and the budget is spent.
- A borrowed source agent is never nudged: the nudge is not the user's session
  to interrupt.
- A wake-up claimed but never settled (the worker died mid-delivery) is settled
  as `uncertain` and the step is surfaced. It is never re-sent, because the
  prompt may already have reached the terminal.

## Attention — compatible plugin contract

mcode 0.6.13 accepts the Claude-compatible plugin format used by Buildmesh.
The older manifest-free `io.minimax.mcode/hooks/hooks.json` layout is not the
compatible plugin registration used here. MiniMax's current native plugin
format also exists; it does not require migrating this working compatible plugin.

- Hook stdin JSON (captured live from a `Stop` callback): `hook_event_name`,
  `session_id`, `prompt_id`, `transcript_path`, `cwd`, `permission_mode`,
  `effort`, `last_assistant_message`.
- Events: `PreToolUse`, `PostToolUse`, `SessionStart`, `SessionEnd`,
  `UserPromptSubmit`, `Stop`, `PreCompact`, `PostCompact`, `SubagentStart`,
  `SubagentStop`, `PermissionRequest` (shipped 0.6.13 parser).
- Buildmesh keeps hooks **inlined** on `.claude-plugin/plugin.json` for
  compatibility with its validated 0.4.12 installation, each event mapping
  to `[{ "matcher": "*", "hooks": [{ "type": "command", "command", "args",
  "timeout" }] }]`. The 0.6.13 compatible loader also accepts referenced hook
  documents and defaults to `hooks/hooks.json` when the manifest omits `hooks`;
  inline hooks are no longer required. This is verified in the shipped
  0.6.13 loader (`chunk-H6JEREKX.js`, `yP` → `UV` → `Jet`), not by the native
  probe, which used inline hooks.

### What Buildmesh provisions

`McodeAdapter::provision_attention_hooks` writes
`<dataDir>/plugins/io.buildmesh.attention/.claude-plugin/plugin.json` with
`SessionStart`, `UserPromptSubmit`, `Stop` and `PermissionRequest` handlers.
Submit supplies work-resumed evidence; completion retains native pending-work
reconciliation. Three mcode constraints shape the
write, and all three were found the hard way:

1. The manifest must live at `.claude-plugin/plugin.json`. A plugin directory
   without one is skipped **silently** by the scan — no plugin, no diagnostic —
   which made the earlier `hooks/hooks.json`-only layout dead on arrival.
2. `command` + `args` run with **no shell interpretation**, so the invocation
   names a shell explicitly (`cmd.exe /c …` on Windows, `sh -c …` on POSIX) and
   hands it a single curl line.
3. mcode `env_clear()`s the `BUILDMESH_*` variables before running a hook (the
   same constraint Codex has), so the callback URL **bakes** the loopback ports
   of the Buildmesh instances running on the machine. A live run confirmed
   `%BUILDMESH_PORT%` arriving verbatim.

The merge is additive (sibling events and user handlers round-trip) and
idempotent; a malformed user manifest fails closed rather than being
overwritten; and an unresolvable data dir returns `Ok(())` with no side effects.

### Validation evidence

Against the installed `@minimax-ai/code` **0.4.12** on Windows, 2026-09-20:

- `Stop` fires at turn end and POSTs the mcode envelope to
  `/api/attention/<node-id>`, confirmed from **both** `mcode exec` and the
  **interactive TUI driven through a ConPTY** (a genuine model turn completed):
  `{"stop_hook_active":false,"last_assistant_message":"OK","hook_event_name":"Stop","session_id":"mvs_…","prompt_id":"turn_…","transcript_path":"…","permission_mode":"auto","effort":{"level":"medium"}}`.
- The node-id URL above records that 0.4.12 validation setup. The corrected
  adapter now provisions `/api/attention/mcode`. The newer native 0.6.13
  command-level capture above observes SessionStart, submit and completion;
  it does not exercise this HTTP route.
- Each run produced exactly one `Stop` POST — no duplicate turns.
- The attention route's parser already accepts this envelope (`hook_event_name`,
  `session_id`, `transcript_path`, `last_assistant_message`), so no route change
  was needed.
- `mcode`'s hook cache (`<dataDir>/v2/plugin-hook-cache/`) materialises the
  plugin once the manifest exists, which is the cheap way to tell "loaded" from
  "silently skipped".

Multi-node behaviour was probed directly rather than assumed: a session bound
to node 7, whose manifest was overwritten mid-session with node 99, kept posting
to node 7 — mcode snapshots a plugin's hooks at process start. Still not
exercised: two nodes sharing one worktree directory, a live WSL guest, and a
Buildmesh restart onto a different port (see Known limits).

### Advertised capability

`requires_attention_hook = true`, with `AttentionCapability::Hook`:
`events: [TurnCompleted]`, `launch_mode: SkipPermissions`,
`min_version: "0.4.12"`, `trust: None`.

`PermissionRequest` is provisioned but **not advertised**. Buildmesh launches
mcode in **Full Access** (`permissionMode: bypassPermissions`, pinned into
`<dataDir>/config.yaml` on every spawn), which auto-approves — every observed
hook envelope reports `"permission_mode": "auto"` under the old default launch,
and even a shell command ran without a prompt — so a permission signal is
impossible by construction under our launch, exactly like Cursor under
`--force`. The contract used to hold only by coincidence of mcode's compiled
default; it is now true by construction, and MiniMax's own recommendation for
unattended runs. If a future launch enables ask mode, the handler is already
provisioned.

### Pinning Full Access

The installed TUI help exposes no permission flag. `--permission` exists on
`mcode exec` only, which Buildmesh never spawns for interactive nodes, and mcode reads no
environment variable for the mode (enumerated across the installed 0.5.5
bundle). The sole lever is the top-level `permissionMode` key in
`<dataDir>/config.yaml`, validated against
`default | bypassPermissions | auto | off` with a compiled default of `auto`.

`pin_permission_mode` therefore edits that key on every spawn, next to the
attention plugin provisioning, resolving the data dir through the same
WSL-aware path so a guest mcode is configured from the guest's config.

- **The edit is surgical, one line at a time.** A parse-and-re-serialise
  round-trip would strip every comment and reflow the file; this rewrites only
  the `permissionMode` line (preserving its trailing comment and column
  alignment) or appends one. No YAML dependency is needed.
- **It never fabricates a config.** mcode bootstraps its own config on first
  run and *skips* that bootstrap when the file already exists, so creating a
  file containing only this key would suppress the bootstrap and leave the CLI
  with no `provider` block at all. An absent config is left absent; the next
  spawn (or the user's first run) finds the file and pins it.
- **Only the top-level key.** An indented `permissionMode` belongs to another
  mapping and is left alone, as are `permissionModes:`-style longer siblings
  and commented-out keys.
- **It is idempotent.** A config already at Full Access is not rewritten, so a
  steady-state spawn does no I/O.
- **Failures never fail a launch.** An unreadable config is logged and skipped;
  the node starts in mcode's own default mode rather than aborting.

**Known limit — machine-global.** Unlike the node-scoped `node_id` baked into
the plugin manifest, this key is shared with the user's own standalone `mcode`
sessions, which will also run in Full Access until the key is edited back. It
is the only scope the CLI allows: scoping per node would need
`MINIMAX_DATA_DIR` redirected at a Buildmesh-owned dir, which relocates auth
and sessions too and breaks the transcript reader.

`MissingAttentionHook` no longer fires for mcode, so
`circuit::compatibility::evaluate` allows it (with worktrees on); pinned by
`compute_for_mesh_allows_mcode_via_attention_hook`.

### Known limits

- **One machine-global manifest.** The plugin path is the user's home,
  not the worktree. All nodes provision the same `/api/attention/mcode` URL.
  The route requires a native session id, matching workspace, live process,
  and current process generation. Known sessions cannot claim another node;
  simultaneous fresh nodes sharing one workspace are rejected as ambiguous.
  Older numeric callbacks use the same resolution, ignoring their baked node id.
- **A shared manifest names each instance, not one.** mcode clears `BUILDMESH_*`
  and there is no scoped plugin dir, so the port cannot be resolved at run time
  like every other harness. Baked the spawning instance's port, the last
  MiniMax spawn stole the other instances' callbacks and their nodes stayed
  `Running` forever (issue #2343). The manifest now lists the port of each
  Buildmesh instance listening on the machine (`http::instances`), one curl
  invocation and one stdin read serving all of them; the node-independent route
  lets the owning instance accept a callback and the others discard it as
  unknown. Verified against two live listeners with a dead port between them —
  not against the installed TUI, so live multi-instance delivery is still
  unvalidated.
- **WSL-guest mcode now fails provisioning loudly when it cannot deliver.**
  Reaching the Windows-side Buildmesh from the guest needs mirrored networking,
  so `provision_attention_hooks` preflights `wslinfo --networking-mode`
  (mirroring `grok.rs`) and returns an actionable error — surfacing as
  `SignalHealth::Unavailable` — instead of installing a hook that could only
  fail silently. The `WindowsInterop` direction needs no preflight: its relay
  runs `curl` back inside the guest, where the Linux-side listener's own
  loopback is reachable. The WSL leg is still not *validated* on a live guest
  from this host.
- The baked ports mean a node that survives a Buildmesh restart onto a different
  HTTP port keeps the old port until it is re-spawned. The port is stable for
  the life of the process (`RESOLVED_HTTP_PORT`) and provisioning runs on every
  spawn, so a re-spawn re-points it — the same class of limitation Codex has.
  A port that no longer answers is dropped from the list when the next spawn
  provisions (a TCP-connect probe), so a crashed sibling's entry cannot
  accumulate.
- Permission, question, and background-work signals are **not** claimed: none
  was observed. A passive `mcode_watcher` over `messages.jsonl` run boundaries
  (the Muse / Command Code pattern) remains the fallback if the plugin path
  proves unreliable.

## Session identity — native callbacks

mcode assigns `mvs_<hex>` ids. Buildmesh provisions `SessionStart`,
`UserPromptSubmit`, `Stop`, and `PermissionRequest` handlers. Native 0.6.13
exec and interactive TUI captures establish that SessionStart delivers
`session_id`, `cwd`, and `transcript_path` at first-turn admission, before
UserPromptSubmit and Stop. The handler captures identity without publishing
Ready. These command-level captures do not establish end-to-end delivery to
Buildmesh's shared HTTP route. Until a callback carrying identity is delivered, a new node has
no `cli_session_id`, which startup resume and transcript lookup require.
Session/workspace routing and the conditional database write reject ambiguous
or duplicate owners, but do not establish process identity.

PTY capture remains off. The former time-window manifest poller is removed:
one observed manifest can be the first of two simultaneous spawns and is not
proof of ownership. Resuming a known id remains supported; a suspended node
without an id cannot safely infer one from timestamps. Historical duplicate
ownership is rejected rather than silently rewritten. See
[runs 276/277](../archive/2026-10/circuit-runs-276-277.md) for the observed failure.
