---
name: agy-harness-capabilities
description: Antigravity CLI (agy) capability review against Buildmesh's harness contract — attention hooks, skip-permissions, resume, and effort
metadata:
  type: reference
  harness: agy
  min_version: 1.0.0
  tested_version: 1.2.13
  date: 2026-09-29
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
| **Session Resume** | **Active (#1499)** | `--conversation <uuid>`; self-assigned UUIDv4 captured by the brain-directory poller (`services::agy_session`, primary — PTY capture is disabled, the TUI prints no UUID), `Stop`-hook `conversationId` as secondary |
| **Reasoning Effort** | **Active (#1286)** | Closed vocabulary `low`, `medium`, `high` via `--effort` |
| **Native Sandbox** | **Active (#1287)** | Forwarded via `--sandbox` when mesh sandbox toggle is on |
| **Transcript Reader** | **Active (#1283)** | `TranscriptFormat::Agy` reads `~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript.jsonl` |

## Session discovery during review

Session discovery first uses transcript mtimes to identify plausible
conversations, then queries only those IDs from the read-only
`conversation_summaries.db` beside `brain`. URI JSON is decoded only for matching
transcripts, and SQLite closes before transcript reads. A single distinct
decodable file URI can identify the launch workspace; unsupported URI members
are ignored. More than one distinct decodable file root is ambiguous and falls
back to transcript Cwd. Review commands can use the source agent's worktree, so
their `Cwd` must not override an unambiguous recorded launch workspace. A
missing or unreadable database, missing row, blank value, or row with no
decodable file root also uses transcript Cwd; SQL NULL is handled defensively
even though the observed schema declares the column NOT NULL. Unparsable JSON,
non-UTF-8 text, or an unexpected SQLite type leaves a matching row unverified.
Root matching still uses the existing directory comparison: WSL guest paths and
host UNC paths are not reconciled here, including for the transcript Cwd
fallback. Creation-time windows and unique-candidate checks still apply.

Run 259 exposed this on 2026-09-29: reviewer 4613 had an approval report but no
captured identity because its commands targeted the source worktree. Restoring
only the proven session identity let the running worker complete the circuit.
The native log also recorded a Stop-hook JSON response error; discovery recovery
does not repair that hook or establish complete native lifecycle coverage.

## Primary Sources

1. **AGY CLI Reference & Live Binary**: `agy 1.2.13` (`agy --version`, `agy --help`, `agy changelog`).
2. **AGY Customization System**: `.agents/hooks.json`, `.agents/rules/`, progressive disclosure.
3. **Buildmesh AGY Adapter**: `src-tauri/src/agent/provider/adapters/agy.rs`.
4. **Attention Route**: `src-tauri/src/http/routes/attention.rs`.
5. **Issues**: #1283 (transcripts), #1285 (hooks), #1286 (effort), #1287 (sandbox), #1367 (validation and hardening).
6. **AGY Session Summary Store**: the local `conversation_summaries.db` was
   inspected read-only alongside `agy 1.2.13` on 2026-09-29. The schema has a
   TEXT primary-key `conversation_id` and NOT NULL TEXT `workspace_uris`.
   Aggregate inspection found 637 rows, including 39 blank values and 598
   single-root arrays; no multi-root arrays appeared in this sample. No
   conversation IDs or paths were retained. Because this sample provides no
   evidence that extra roots in a multi-root row are launch-scoped, the parser
   treats multiple distinct decodable file roots as ambiguous and falls back
   to transcript Cwd; no live multi-root behavior is claimed.

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
  "transcriptPath": "F:\\src\\repo\\.gemini\\antigravity-cli\\transcript.jsonl",
  "artifactDirectoryPath": "F:\\src\\repo\\.gemini\\antigravity-cli\\artifacts",
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
2. **Atomic Writes**: `ensure_hooks_json` writes via a unique PID+counter `.tmp` file and performs an atomic fsynced rename, preventing file corruption across concurrent spawns.
3. **Namespace Isolation**: `buildmesh-attention` lives as a distinct top-level object key in `.agents/hooks.json`. User-defined hooks and sibling tools are preserved intact.
4. **Failure Observability**: Hook injection failures emit a `provider-error` warning and log detailed diagnostics rather than continuing silently.

---

## 5. Circuit Lifecycle & Ownership Contract (issue #1901)

Validated against `agy 1.2.11` on Windows (authenticated CLI; provisioned
hook lists as `enabled` under `/hooks`). What Buildmesh can and cannot
establish for Antigravity Circuit execution:

| Fact | Source | Status |
|---|---|---|
| Supported version / platform | `agy 1.2.11`, Windows interactive PTY session | Validated |
| Turn settled vs background-busy | `Stop` hook `fullyIdle: true` vs `false` | Validated (hook source) |
| Session identity | `conversationId` (UUID, lowercase-canonicalized) fenced against the node's stored session and incarnation | Validated with stated limits |
| Input-stamp fence | None — without a `UserPromptSubmit` turn-start binding, persistence strips the input stamp, so receipts are never stale-marked | Unavailable by design |
| Per-turn token | None — `executionNum` is an opaque 0-based step counter, not identity (retained only so consecutive turns hash to distinct receipt source ids) | Unavailable by design |
| Child/background registry | None — the payload carries no task/cron lists | Unavailable |
| Inline final report | None — `Stop` carries no assistant text | Unavailable |
| Human waits (permission/question) | No hook installed under `--dangerously-skip-permissions` | Unavailable |

Consequences for Circuit execution (`services::circuit_worker::native_hooks`,
`observer_policy::for_provider("agy")`):

1. Every `Stop` receipt normalizes to `ForegroundTerminated` (or `Yielded`
   when `fullyIdle: false`) plus `OwnershipUnavailable`. Receipts are
   session/incarnation-fenced but never turn-fenced and never input-fenced,
   hence never authoritative: they are visible evidence in run history and
   can never complete a step. Owned-work coverage stays `Unverified`.
2. Freshness bounds are unchanged from the default (30s yielded budget). A
   replaced conversation is rejected on session mismatch; byte-identical
   redeliveries dedupe by source identity while consecutive turns stay
   distinct via the retained `executionNum`; and a deleted node consumes its
   receipt without effect. Stale-marking on input overtake does not apply:
   with no persisted input stamp the receipt can never be stale-marked.
3. Subagent `Stop`s carry their own `conversationId`, so they fence as a
   different session and can never complete the parent's turn. Malformed
   payloads (including non-UUID conversation ids) and non-`Stop` events
   parse to nothing under the `agy` provider, and `agy` bytes parse to
   nothing under harnesses that own no AGY adapter. `terminationReason` is
   retained on the receipt as triage telemetry only.
4. Boundary: `-p` / `--print` headless runs emit **no** `Stop` hook (two
   controlled runs with a live catcher, zero deliveries), so hook evidence
   covers only Buildmesh-launched interactive sessions. A `hooks.json`
   written with a UTF-8 BOM is silently dropped by the harness — the
   provisioner writes BOM-free JSON.
