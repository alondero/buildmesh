---
name: mcode-harness-capabilities
description: MiniMax Code (mcode) capability review against Buildmesh's harness contract — transcript reader evidence and the validated attention hook
metadata:
  type: reference
  harness: mcode
  mcode_version: 0.4.12 (@minimax-ai/code)
  date: 2026-09-20
  attention_validation: validated 2026-09-20 against installed 0.4.12 —
    Stop delivered from a live TUI; requires_attention_hook flipped to true
---

# MiniMax Code harness capabilities vs Buildmesh

Review of what the `mcode` binary actually exposes, versus what Buildmesh's
Mcode adapter advertises and uses. Both primary concerns have landed:
**transcript understanding** (`TranscriptFormat::Mcode`) and **attention
hooks** (the provisioned Agent-Plugin, validated against a live 0.4.12 TUI —
issue #1797). The Autopilot gate is open for mcode.

## Sources (primary only)

| Source | What it is |
|---|---|
| `@minimax-ai/code@0.4.12` npm bundle (`cli.js` + `chunks/*.js` strings) | Shipped CLI: data-dir resolution, session layout, plugin scan, hook payload |
| Installed `mcode 0.4.12` driven on Windows, 2026-09-20 | Live `exec` and interactive-TUI runs against a local listener (issue #1797) |
| `MiniMax-AI/minimax-code-plugins` (`proposals/hooks-v0.4-spec.md`, `docs/plugin-compatibility.md`) | The mcode 0.4.0+ plugin format: `.claude-plugin/plugin.json`, inline `hooks` |
| `MiniMax-AI/minimax-code-plugins` (`proposals/hooks-detailed-spec.md`, `examples/hello-mcode-hooks`) | The superseded v0.3.x Agent-Plugin format (`io.minimax.mcode/hooks/hooks.json`) |
| `src-tauri/src/agent/provider/adapters/mcode.rs` | Current Buildmesh adapter |
| `src-tauri/src/services/transcript_reader/adapters/mcode.rs` | Current Buildmesh reader |
| `src-tauri/src/agent/capabilities.rs`, `autopilot/compatibility.rs` | Harness contract and Autopilot gate |

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
| `self_assigns_session_id` | `true` | mcode mints its own ids; capture is the post-spawn manifest poller, never PTY |
| `supports_prefill` | `true` | Trailing positional `[prompt]`, no `--prefill` flag |
| `supports_model_override` | `false` | Issue #1179: `--model` exists only on `mcode exec`, never the launched TUI |
| `effort_control` | `None` | Same reason — the TUI rejects effort flags |
| `requires_attention_hook` | `true` | `Stop` delivered from a live 0.4.12 TUI (#1797); `SkipPermissions`, `TurnCompleted` only — see below |
| `attention_capability` | `Hook { events: [turn_completed], launch_mode: skip_permissions, min_version: "0.4.12" }` | Buildmesh launches mcode with an auto-approving policy, so no permission signal is claimed |
| `produces_readable_transcript` | `true` | Canonical `messages.jsonl` via `TranscriptFormat::Mcode` (this change) |
| Shell | `WindowsShell::Cmd` on Windows, `Direct` elsewhere | Correct — `.cmd` shim on Windows, native binary on macOS/Linux |
| Launch mode | Interactive TUI | Correct — PTY backend supports full-screen rendering |

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
- Each run produced exactly one `Stop` POST — no duplicate turns.
- The attention route's parser already accepts this envelope (`hook_event_name`,
  `session_id`, `transcript_path`, `last_assistant_message`), so no route change
  was needed.
- `mcode`'s hook cache (`<dataDir>/v2/plugin-hook-cache/`) materialises the
  plugin once the manifest exists, which is the cheap way to tell "loaded" from
  "silently skipped".

Not exercised here: two nodes sharing one worktree directory, and a Buildmesh
restart onto a different port (see Known limits).

### Advertised capability

`requires_attention_hook = true`, with `AttentionCapability::Hook`:
`events: [TurnCompleted]`, `launch_mode: SkipPermissions`,
`min_version: "0.4.12"`, `trust: None`.

`PermissionRequest` is provisioned but **not advertised**. Buildmesh launches
mcode with its default permission policy, which auto-approves — every observed
hook envelope reports `"permission_mode": "auto"`, and even a shell command ran
without a prompt — so a permission signal is impossible by construction under
our launch, exactly like Cursor under `--force`. If a future launch enables ask
mode, the handler is already provisioned.

`MissingAttentionHook` no longer fires for mcode, so
`autopilot::compatibility::evaluate` allows it (with worktrees on); pinned by
`compute_for_mesh_allows_mcode_via_attention_hook`.

### Known limits

- The baked URL means a node that survives a Buildmesh restart onto a different
  HTTP port keeps the old port until it is re-spawned — the same limitation
  Codex has. Provisioning runs on every spawn, so a re-spawn re-points it.
- Permission, question, and background-work signals are **not** claimed: none
  was observed. A passive `mcode_watcher` over `messages.jsonl` run boundaries
  (the Muse / Command Code pattern) remains the fallback if the plugin path
  proves unreliable.
- WSL-guest mcode (`EnvType::Wsl` / `WindowsInterop`) is **not** validated here.
  A guest reaches the Windows-side Buildmesh only under mirrored networking —
  the same constraint Grok's HTTP hooks carry — and, unlike Grok, the mcode
  provisioner does not preflight the networking mode. A WSL node without
  mirrored networking therefore writes the plugin but never delivers, and the
  failure is silent (the curl error is swallowed). A network-mode preflight,
  mirroring `grok.rs`, is the follow-up if WSL parity is wanted.

## Session identity — manifest-scan capture (issue #1798)

mcode auto-assigns session ids; PTY-output capture is off
(`captures_session_id_from_pty = false` — no banner shape is verified, and
leaving it on would risk binding a stray UUID from tool output). Capture is
driven by the post-spawn manifest poller (`services::mcode_session`, the
Command Code pattern): `after_fresh_spawn` binds the fresh `sessionId`
through the shared live recovery, and `recover_suspended_session_id`
rebinds archived nodes after restart. Matching is time-window only with
single-candidate binding — the manifest carries no verified workspace
anchor, so two fresh manifests bind nothing rather than risk cross-wiring
sessions.
