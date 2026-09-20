---
name: mcode-harness-capabilities
description: MiniMax Code (mcode) capability review against Buildmesh's harness contract — transcript reader evidence, attention-hook surface, and the remaining Autopilot gap
metadata:
  type: reference
  harness: mcode
  mcode_version: 0.4.12 (@minimax-ai/code, npm bundle strings)
  date: 2026-09-19
---

# MiniMax Code harness capabilities vs Buildmesh

Review of what the `mcode` binary actually exposes, versus what Buildmesh's
Mcode adapter advertises and uses. Primary concerns: **transcript
understanding** (landed — `TranscriptFormat::Mcode`) and **attention hooks**
(open — the Autopilot gate).

## Sources (primary only)

| Source | What it is |
|---|---|
| `@minimax-ai/code@0.4.12` npm bundle (`cli.js` + `chunks/*.js` strings) | Shipped CLI: data-dir resolution, session layout, hook payload/output protocol |
| `mcode --help` shape via README (`mcode [prompt]`, `--session [id]`, `--continue`, `exec`, `plugin`) | Public CLI surface |
| `MiniMax-AI/minimax-code-plugins` (`proposals/hooks-detailed-spec.md`, `examples/hello-mcode-hooks`) | Agent Plugins 1.0 portable Hooks preview: `hooks.json` entries, per-event scripts |
| `src-tauri/src/agent/provider/adapters/mcode.rs` | Current Buildmesh adapter |
| `src-tauri/src/services/transcript_reader/adapters/mcode.rs` | Current Buildmesh reader |
| `src-tauri/src/agent/capabilities.rs`, `autopilot/compatibility.rs` | Harness contract and Autopilot gate |

No live `mcode` binary was available on this host; every claim below traces
to bundle strings or the linked repos, never to guessed PTY output.

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
| `requires_attention_hook` | `false` | No hook provisioned — honest-empty, see below |
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

## Attention — open (the Autopilot gap)

mcode ships a Claude-compatible hook surface (Agent Plugins 1.0 preview,
`mcode 0.2.4+` per the plugins repo):

- Hook stdin JSON: `hook_event_name`, `session_id`/`sessionId`,
  `turn_id`/`turnId`, `transcript_path`, `cwd`, `model`, `permission_mode`,
  `effort`. Events include `SessionStart`, `SessionEnd`, `UserPromptSubmit`,
  `PreToolUse`, `PermissionRequest`, `PostToolUse`, `SubagentStart/Stop`,
  `Stop`, `PreCompact`, `PostCompact`; `sourceFormat` covers `MINIMAX`,
  `CODEX`, and `CLAUDE` payloads.
- Hook stdout JSON: `{decision: "block", reason, hookSpecificOutput:
  {hookEventName, additionalContext}, continue, continuePrompt, …}`.
- Hooks ship inside plugins (`<plugin>/hooks/hooks.json` entries plus
  per-event scripts, e.g. `io.minimax.mcode/hooks/scripts/<event>.ps1`).

Buildmesh provisions **nothing** here yet: no plugin is installed, no hook
command is registered, and no `Stop`→`turn_completed` delivery has been
validated against a live TUI. Per the primer rule ("native hooks are
provisioned only where the installed harness contract is verified"), the
adapter stays honest-empty (`requires_attention_hook = false`,
`attention_capability = None`) instead of guessing from PTY output.

Consequence: `autopilot::compatibility::evaluate` still returns
`MissingAttentionHook { harness_id: "mcode" }` — mcode nodes get digests and
picker support, but Autopilot circuits stay closed until the follow-up lands:

1. Ship a Buildmesh attention plugin (Stop + PermissionRequest entries
   POSTing to `/api/attention/<node-id>`, Claude-hook curl shape) or find
   the user-config hook file the TUI reads without a plugin install.
2. Implement `provision_attention_hooks` (idempotent merge, additive like
   Kimi/Grok — never clobber user plugins).
3. Validate end-to-end against a live `mcode` TUI (Stop fires at turn end,
   no duplicate turns), then flip `requires_attention_hook` and advertise
   the `Hook` capability with events + min version.
4. A passive `mcode_watcher` over `messages.jsonl` run boundaries (Muse /
   Command Code pattern) is the fallback if plugin hooks prove unreliable.

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
