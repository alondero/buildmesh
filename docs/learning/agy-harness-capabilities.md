---
name: agy-harness-capabilities
description: Antigravity CLI (agy) capability review against Buildmesh's harness contract — attention hooks, skip-permissions, resume, and effort
metadata:
  type: reference
  harness: agy
  min_version: 1.0.0
  tested_version: 1.1.22
  date: 2026-08-30
---

# Antigravity (agy) harness capabilities vs Buildmesh

Review of Antigravity CLI (`agy`) integration with Buildmesh, covering lifecycle hook delivery, execution models, and capability boundaries (issues #1285, #1286, #1287, #1367).

## Summary & Status

| Area | Status | Contract Details |
|---|---|---|
| **Attention Hook Delivery** | **Validated & Hardened (#1367)** | `.agents/hooks.json` under `buildmesh-attention` namespace; `Stop` hook forwards stdin to `/api/attention/:session_id` |
| **Turn Completion Signal** | **Active** | `fullyIdle: true` signals turn settled -> `Decision::Mark` |
| **Background Yield** | **Active** | `fullyIdle: false` signals background task running -> `Decision::SuppressPendingBackground` |
| **Permission Gating** | **Skipped by Design** | `--dangerously-skip-permissions` active; `PreToolUse` omitted to prevent synchronous blocking tool execution gates |
| **Workspace Trust** | **Pre-provisioned** | `ensure_trusted` populates `~/.gemini/antigravity-cli/settings.json` before spawn |
| **Session Resume** | **Active** | `--conversation <uuid>`; self-assigned UUIDv4 persisted from hook/PTY capture |
| **Reasoning Effort** | **Active (#1286)** | Closed vocabulary `low`, `medium`, `high` via `--effort` |
| **Native Sandbox** | **Active (#1287)** | Forwarded via `--sandbox` when mesh sandbox toggle is on |
| **Transcript Reader** | **Active (#1283)** | `TranscriptFormat::Agy` reads `~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript.jsonl` |

## Primary Sources

1. **AGY CLI Reference & Live Binary**: `agy 1.1.22` (`agy --help`, `agy changelog`).
2. **AGY Customization System**: `.agents/hooks.json`, `.agents/rules/`, progressive disclosure.
3. **Buildmesh AGY Adapter**: `src-tauri/src/agent/provider/adapters/agy.rs`.
4. **Attention Route**: `src-tauri/src/http/routes/attention.rs`.
5. **Issues**: #1283 (transcripts), #1285 (hooks), #1286 (effort), #1287 (sandbox), #1367 (validation and hardening).

---

## 1. Attention Hook Contract & Delivery

Antigravity executes external shell commands at specific points during the agent execution loop via `.agents/hooks.json` at the workspace root.

### Hook Structure in `.agents/hooks.json`

```json
{
  "buildmesh-attention": {
    "Stop": [
      {
        "type": "command",
        "command": "curl.exe -sf --connect-timeout 1 --max-time 2 -X POST -H \"Content-Type: application/json\" --data-binary @- http://localhost:%BUILDMESH_PORT%/api/attention/%BUILDMESH_SESSION_ID% >nul 2>nul & echo {\"decision\":\"allow\"}"
      }
    ]
  }
}
```

### Stdin Payload Contract (camelCase)

When `Stop` fires, AGY pipes a JSON payload to the command's stdin:

```json
{
  "conversationId": "550e8400-e29b-41d4-a716-446655440000",
  "executionNum": 1,
  "terminationReason": "model_stop",
  "error": "",
  "fullyIdle": true,
  "workspacePaths": ["F:\\src\\repo"],
  "transcriptPath": "F:\\src\\repo\\.gemini\\antigravity\\transcript.jsonl",
  "artifactDirectoryPath": "F:\\src\\repo\\.gemini\\antigravity\\artifacts",
  "modelName": "gemini-3.7-flash"
}
```

### Stdout Decision Contract

AGY parses the hook process's stdout as JSON:
- `{"decision":"allow"}` (or empty `{}`): Allows the agent execution to terminate or stop.
- `{"decision":"continue", "reason":"..."}`: Blocks the stop and forces the agent back into the loop.

Buildmesh returns `{"decision":"allow"}` unconditionally (fail-open) so agent turns are never blocked even if Buildmesh is unreachable.

---

## 2. Why `Stop` Only Under `--dangerously-skip-permissions`

Buildmesh launches AGY with `--dangerously-skip-permissions` to allow automated agent execution without blocking for interactive manual approvals in the terminal for every command.

1. **`PreToolUse` is a synchronous decision gate**, requiring a structured `{ "decision": "allow" | "deny" | "ask" | "force_ask" }` response before each tool executes. It is not an asynchronous notification event.
2. Under `--dangerously-skip-permissions`, no permission prompts occur. Injecting `PreToolUse` would only add unnecessary synchronous curl execution overhead to every tool call.
3. Therefore, `Stop` is the sole lifecycle event required for turn completion and background detection.

---

## 3. Background Work vs Completed Turns

AGY differentiates between background execution and settled turns via the `fullyIdle` boolean:
- **`fullyIdle: false`**: The agent has yielded a turn, but background tasks (e.g. background bash tasks, subagents) are still in flight. Buildmesh classifies this as `Decision::SuppressPendingBackground` — the Node Turn is published for naming/autopilot, but `Needs attention` is **not** rendered in the UI.
- **`fullyIdle: true`**: All tasks have settled and the model finished its response. Buildmesh classifies this as `Decision::Mark` — the node flips to `awaiting_input` and alerts the user.

---

## 4. Provisioning & Workspace Trust Discipline

1. **Pre-Launch Provisioning**: `ensure_trusted` (in `workspace_trust.rs`) and `inject_attention_hook` (in `agy.rs`) run **before** spawning the child process in `spawn_agent_inner`. This eliminates race conditions where AGY booted before `.agents/hooks.json` or `trustedWorkspaces` existed on disk.
2. **Atomic Writes**: `ensure_hooks_json` writes to `hooks.json.tmp` and performs an atomic rename, preventing file corruption across concurrent spawns.
3. **Namespace Isolation**: `buildmesh-attention` lives as a distinct top-level object key in `.agents/hooks.json`. User-defined hooks and sibling tools are preserved intact.
4. **Failure Observability**: Hook injection failures emit a `provider-error` warning and log detailed diagnostics rather than continuing silently.
