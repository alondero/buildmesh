---
name: mcode-harness-capabilities
description: MiniMax Code (mcode) capability review against Buildmesh's harness contract — transcript reader evidence and the validated attention hook
metadata:
  type: reference
  harness: mcode
  mcode_version: 0.6.5 (model CLI); 0.4.12 (attention validation)
  date: 2026-10-09
  attention_validation: Stop validated 2026-09-20 against installed 0.4.12;
    SessionStart provisioned for identity capture but delivery unvalidated
    against the installed TUI as of 2026-10-01
---

# MiniMax Code harness capabilities vs Buildmesh

Review of what the `mcode` binary actually exposes, versus what Buildmesh's
Mcode adapter advertises and uses. Both primary concerns have landed:
**transcript understanding** (`TranscriptFormat::Mcode`) and **attention
hooks** (the provisioned Agent-Plugin, validated against a live 0.4.12 TUI —
issue #1797). The Circuit compatibility gate is open for mcode.

## Sources (primary only)

| Source | What it is |
|---|---|
| `@minimax-ai/code@0.4.12` npm bundle (`cli.js` + `chunks/*.js` strings) | Shipped CLI: data-dir resolution, session layout, plugin scan, hook payload |
| Installed `mcode 0.4.12` driven on Windows, 2026-09-20 | Live `exec` and interactive-TUI runs against a local listener (issue #1797) |
| Installed `mcode 0.6.5` on Windows, 2026-10-09 | `mcode --version` and interactive `mcode --help`: `-m, --model <provider/model>` selects the model for this Session only |
| [MiniMax CLI features](https://agent.minimax.io/docs/cli/features#model-references) | Model reference syntax: `provider/model`, optionally `#variant` |
| `MiniMax-AI/minimax-code-plugins` (`proposals/hooks-v0.4-spec.md`, `docs/plugin-compatibility.md`) | The mcode 0.4.0+ plugin format: `.claude-plugin/plugin.json`, inline `hooks` |
| `MiniMax-AI/minimax-code-plugins` (`proposals/hooks-detailed-spec.md`, `examples/hello-mcode-hooks`) | The superseded v0.3.x Agent-Plugin format (`io.minimax.mcode/hooks/hooks.json`) |
| `src-tauri/src/agent/provider/adapters/mcode.rs` | Current Buildmesh adapter |
| `src-tauri/src/services/transcript_reader/adapters/mcode.rs` | Current Buildmesh reader |
| `src-tauri/src/agent/capabilities.rs`, `circuit/compatibility.rs` | Harness contract and Circuit gate |

Delivery, layout, and payload claims below are backed by the live run; the
scanner and manifest rules are traceable to the shipped bundle and the
first-party plugin spec.

## What Buildmesh wants from a harness

Same checklist as the Grok review (`grok-harness-capabilities.md`): resume,
prefill, model/effort overrides, attention hook, readable transcript,
interactive TUI over PTY, no harness-owned worktree flag.

## What the Mcode adapter advertises

| Flag | Adapter value | Notes |
|---|---|---|
| `supports_resume` | `true` | `resume_args` → `--session <id>` |
| `auto_resume_on_startup` | `true` | |
| `self_assigns_session_id` | `true` | A delivered callback binds session id plus workspace. `Stop` delivery was observed live; `SessionStart` is provisioned but unvalidated, so identity is not established at startup. |
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
Buildmesh validates the `provider/model[#variant]` syntax and accepts only ASCII
letters, digits, `.`, `_` and `-` in each part to prevent Windows Cmd parsing or
expansion. Validation is adapter-owned and runs on configuration/default saves
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

## Attention — validated against a live 0.4.12 TUI (issue #1797)

mcode exposes a twelve-event hook surface. For mcode 0.4.0+ the plugin format
is the Claude-compatible one; the older `io.minimax.mcode/hooks/hooks.json`
document is ignored.

- Hook stdin JSON (captured live from a `Stop` callback): `hook_event_name`,
  `session_id`, `prompt_id`, `transcript_path`, `cwd`, `permission_mode`,
  `effort`, `last_assistant_message`.
- Events: `PreToolUse`, `PostToolUse`, `SessionStart`, `SessionEnd`,
  `UserPromptSubmit`, `Stop`, `PreCompact`, `Notification`, `SubagentStart`,
  `SubagentStop`, `PermissionRequest`, `PermissionDenied`.
- Hooks must be **inlined** on `.claude-plugin/plugin.json`, each event mapping
  to `[{ "matcher": "*", "hooks": [{ "type": "command", "command", "args",
  "timeout" }] }]`.

### What Buildmesh provisions

`McodeAdapter::provision_attention_hooks` writes
`<dataDir>/plugins/io.buildmesh.attention/.claude-plugin/plugin.json` with
`Stop` and `PermissionRequest` handlers. Three mcode constraints shape the
write, and all three were found the hard way:

1. The manifest must live at `.claude-plugin/plugin.json`. A plugin directory
   without one is skipped **silently** by the scan — no plugin, no diagnostic —
   which made the earlier `hooks/hooks.json`-only layout dead on arrival.
2. `command` + `args` run with **no shell interpretation**, so the invocation
   names a shell explicitly (`cmd.exe /c …` on Windows, `sh -c …` on POSIX) and
   hands it a single curl line.
3. mcode `env_clear()`s the `BUILDMESH_*` variables before running a hook (the
   same constraint Codex has), so the callback URL **bakes** the loopback port
   and node id. A live run confirmed `%BUILDMESH_PORT%` arriving verbatim.

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
  adapter now provisions `/api/attention/mcode`; SessionStart delivery from the
  TUI has not yet been observed.
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
  not the worktree. All nodes now provision the same `/api/attention/mcode`
  URL. The route requires a native session id, matching workspace, live process,
  and current process generation. Known sessions cannot claim another node;
  simultaneous fresh nodes sharing one workspace are rejected as ambiguous.
  Older numeric callbacks use the same resolution, ignoring their baked node id.
- **WSL-guest mcode now fails provisioning loudly when it cannot deliver.**
  Reaching the Windows-side Buildmesh from the guest needs mirrored networking,
  so `provision_attention_hooks` preflights `wslinfo --networking-mode`
  (mirroring `grok.rs`) and returns an actionable error — surfacing as
  `SignalHealth::Unavailable` — instead of installing a hook that could only
  fail silently. The `WindowsInterop` direction needs no preflight: its relay
  runs `curl` back inside the guest, where the Linux-side listener's own
  loopback is reachable. The WSL leg is still not *validated* on a live guest
  from this host.
- The baked port means a node that survives a Buildmesh restart onto a different
  HTTP port keeps the old port until it is re-spawned. The port is stable for
  the life of the process (`RESOLVED_HTTP_PORT`) and provisioning runs on every
  spawn, so a re-spawn re-points it — the same class of limitation Codex has.
- Permission, question, and background-work signals are **not** claimed: none
  was observed. A passive `mcode_watcher` over `messages.jsonl` run boundaries
  (the Muse / Command Code pattern) remains the fallback if the plugin path
  proves unreliable.

## Session identity — native callbacks

mcode assigns `mvs_<hex>` ids. Buildmesh provisions `SessionStart`, `Stop`, and
`PermissionRequest` handlers. Static inspection of the installed 0.4.12 bundle
shows a SessionStart payload schema with `session_id`, `cwd`, and
`transcript_path`; delivery from the installed TUI remains unvalidated. The
handler is provisioned to capture identity without publishing Ready. Live
validation established that `Stop` also delivers the session id, so identity
can be captured when the first turn completes; startup-time `SessionStart`
delivery is still unproven. The removed timestamp poller has no verified
replacement. Until a callback carrying identity is delivered, a new node has
no `cli_session_id`, which startup resume and transcript lookup require.
Session/workspace routing and the conditional database write reject ambiguous
or duplicate owners, but do not establish process identity.

PTY capture remains off. The former time-window manifest poller is removed:
one observed manifest can be the first of two simultaneous spawns and is not
proof of ownership. Resuming a known id remains supported; a suspended node
without an id cannot safely infer one from timestamps. Historical duplicate
ownership is rejected rather than silently rewritten. See
[runs 276/277](../archive/2026-10/circuit-runs-276-277.md) for the observed failure.
