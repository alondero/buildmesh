---
name: opencode-harness-capabilities
description: Gap analysis of OpenCode CLI capabilities vs Buildmesh's harness contract, with session-ID resume as the primary finding
metadata:
  type: reference
  harness: opencode
  verified_cli_version: 1.18.3
---

# OpenCode harness vs Buildmesh capability contract (2026-08-26)

Primary question: can Buildmesh capture and resume OpenCode session IDs the way it does for Claude Code / Codex / Kimi? Secondary: which other Buildmesh-shaped harness capabilities OpenCode already has that the adapter still advertises as unsupported.

Verified against:

- Live `opencode` 1.18.3 on this machine (`C:\ProgramData\chocolatey\bin\opencode.exe`)
- Official docs: [CLI](https://opencode.ai/docs/cli), [server](https://opencode.ai/docs/server), [plugins](https://opencode.ai/docs/plugins), [permissions](https://opencode.ai/docs/permissions), [TUI](https://opencode.ai/docs/tui), [config](https://opencode.ai/docs/config), [ACP](https://opencode.ai/docs/acp)
- Source on `anomalyco/opencode` `dev`: TUI flags, `validate-session.ts`, session ID schema, session list JSON, SQLite path
- Buildmesh adapter + trait: `src-tauri/src/agent/provider/adapters/opencode.rs`, `provider/mod.rs`, `capabilities.rs`, `spawn.rs`, `session_capture.rs`, `provider_menu.rs`, `services/opencode_session.rs`

## Executive answer

OpenCode **can resume a specific session by ID**. It **cannot** accept a Buildmesh-minted UUID at spawn the way Claude Code does with `--session-id`. It is a **self-assigning** harness, closer to Kimi / MiniMax Code than to Anthropic.

IDs look like `ses_fc52ccfb9ffek1jl23ZwpRuSP7`. Live 1.18.3 rejects UUID-shaped IDs (`Invalid session ID`) and rejects unknown `ses_…` IDs (`Session not found`) rather than creating them.

**First slice (shipped):** adapter is the Kimi shape (`supports_resume` + `self_assigns_session_id` + `--session` / `--model` / `--prompt`). Capture is `opencode session list --format json` filtered by spawn directory. Attention and transcript stay off (Autopilot and the archive picker remain closed).

ADR-0024 previously grouped OpenCode with Anthropic as an assign-mode `--session-id` provider; that sentence is corrected.

---

## What Buildmesh wants from a harness

The `AgentProvider` trait / `HarnessCapabilities` descriptor is the contract. Spawn (`spawn_agent_inner` step 4), Autopilot (`autopilot/compatibility.rs`), the Spawn Menu, and the archived-node picker all consume it.

| Flag / method | What Buildmesh uses it for | Analogous adapters |
|---|---|---|
| `supports_resume` | Enables `SessionIdMode::Assign` or `Resume`; auto-resume on startup; archived-node picker (AND transcript) | Claude, Codex, Cursor, Agy, Kimi, Grok, mcode, dsh, **OpenCode** |
| `auto_resume_on_startup` | `decide_startup_resume` re-spawns Suspended nodes that have a stored `cli_session_id` | Same set as resume |
| `self_assigns_session_id` | Fresh spawn uses `SessionIdMode::None`; capture later | Codex, Agy, Cursor, Kimi, Grok, mcode, **OpenCode** |
| `session_assign_args` | Fresh spawn flags when Buildmesh mints the ID (`--session-id <uuid>`) | Claude, dsh |
| `resume_args` / `spawn_recipe_for_resume` | Re-spawn flags | Per adapter; OpenCode `--session <id>` |
| `supports_model_override` + `model_args` | Mesh / app default `--model` | Claude, Codex, Cursor, Agy, Kimi, Grok, dsh, **OpenCode** |
| `effort_control` / `supports_effort_override` | `--effort` or Codex `-c model_reasoning_effort=` | Claude (closed), Codex (inline) |
| `supports_prefill` + `prefill_args` | Issue spawn, handover, Autopilot prompt delivery | Claude `--prefill`, Codex positional, Agy `--prompt-interactive`, **OpenCode `--prompt`** |
| `requires_attention_hook` + `inject_attention_hook` | Turn-ended / permission POST to localhost | Claude, Codex |
| `produces_readable_transcript` | Coordinator rich layer + **archive resume picker** (`resumable = resume && transcript`) | Claude JSONL, Codex rollouts, Cursor JSONL |
| `captures_session_id_from_pty` | PTY labeled-UUID regex | Codex / Agy; **OpenCode false** |
| `captures_session_id_from_cli_list` | Poll `session list` JSON | **OpenCode true** |

Spawn-mode decision (`spawn.rs`):

```
if supports_resume:
  stored cli_session_id? → Resume
  else if self_assigns → None (capture later)
  else → Assign (mint UUID, write DB, pass session_assign_args)
else → None (never resume, never capture)
```

PTY capture (`session_capture.rs`) only matches a **labeled UUID**. That regex will never match OpenCode's `ses_…` IDs.

Two distinct "resume" products:

1. **Process resume** — kill/restart the PTY with `resume_args(stored_id)`. Needs `supports_resume` + a stored `cli_session_id`. **Shipped.**
2. **Archive resume picker** — `ProviderInfo.resumable = supports_resume && produces_readable_transcript`. Still closed.

Autopilot requires `supports_prefill` **and** `requires_attention_hook`. Prefill is on; attention is still missing, so Autopilot stays blocked.

---

## OpenCode's actual CLI (1.18.3)

Default invocation starts the TUI.

| Flag | Meaning |
|---|---|
| `--session` / `-s` | Session ID **to continue** (must already exist) |
| `--continue` / `-c` | Continue the last session in this directory (do not use for auto-resume) |
| `--fork` | Fork when continuing (requires `--continue` or `--session`) |
| `--prompt` | Initial prompt (prefill) |
| `--model` / `-m` | `provider/model` |
| `--agent` | Agent to use |
| `--auto` | Auto-approve permissions that are not explicitly denied |

Not present on TUI: `--session-id`, `--resume`, `--effort`, `--variant`, `--prefill`, `--title`. `--variant` exists only on `opencode run`. There is **no** `opencode session resume` subcommand; `opencode session` is `list` + `delete` only.

### Session ID format

Schema: must start with `ses`. Generator: `"ses_" + descending()` → 12 hex timestamp chars + 14 base62 chars.

Live `opencode session list --format json` includes `id`, `title`, `updated`, `created`, `projectId`, `directory`. SQLite: `opencode db path` → `%USERPROFILE%\.local\share\opencode\opencode.db`.

### Assign vs resume — settled live

| Invocation | Result (1.18.3) |
|---|---|
| `opencode --session not-a-valid-id` | `Error: Invalid session ID` (exit 1) |
| `opencode --session <uuid>` | `Error: Invalid session ID` (exit 1) |
| `opencode --session ses_000…` (well-formed, unknown) | `Error: Session not found` (exit 1) |

TUI `validateSession` decodes the ID then `session.get({ throwOnError: true })` before the UI starts.

---

## Gap matrix

| Capability | OpenCode has it? | Adapter now | Notes |
|---|---|---|---|
| **Resume by ID** | Yes — `--session` / `-s` | **Yes** | Self-assign, not `--session-id`. |
| **Auto-resume on startup** | Yes once ID is stored | **Yes** | Depends on session-list capture succeeding. |
| **Assign ID at spawn** | **No** | unused | ADR-0024 corrected. |
| **PTY UUID capture** | IDs are `ses_…` | Off | Capture is `session list`. |
| **Model override** | Yes — `--model provider/model` | **Yes** | Format is `provider/model`. |
| **Effort / variant** | `run --variant`; TUI `ctrl+t` | **No** | Honest for TUI spawn. |
| **Prefill** | Yes — TUI `--prompt` | **Yes** | Autopilot still blocked on attention. |
| **Attention / Node Turn** | Plugin `session.idle` / `permission.asked`; SSE `/event` | **No** | Follow-up: inject a plugin. |
| **Readable transcript** | Export JSON, HTTP messages, SQLite | **No** | Follow-up: `TranscriptFormat::OpenCode`. |
| **Auto-approve** | `--auto` | Unmodeled | Follow-up. |
| **Agent selection** | `--agent` | Unmodeled | No trait flag today. |
| **ACP / serve** | `opencode acp`, `serve` / `web` / `attach` | Unmodeled | Not a PTY harness; attach could help capture. |

---

## Session-ID capture

Because OpenCode mints the ID, Buildmesh learns it after spawn and stores `agent_nodes.cli_session_id`.

Shipped: `services::opencode_session` polls `opencode session list --format json -n 50`, matches `directory` to `resolved.spawn_path`, prefers rows created after spawn (10s skew), falls back to newest directory match. IDs must start with `ses_`. PTY capture is off so a labeled UUID cannot poison the column.

### What will not work

1. Mint a UUID and pass `--session-id` / `--session <uuid>` — rejected as invalid.
2. Pass `--session ses_<made-up>` on a fresh spawn — `Session not found`.
3. Enable `self_assigns_session_id` and rely on `session_capture.rs` as-is — regex is UUID-only.

### Hardening options (follow-up)

1. **Project plugin** on `session.created` that POSTs the ID (attention-hook analog). Unambiguous for two Root Nodes in one directory; also unlocks idle/permission events.
2. **Pass `--port <known>`** and `GET /session` / SSE `session.created`.
3. **Read SQLite** (`opencode db path`). Usage code already opens this file.

Do not use `--continue` for auto-resume.

---

## First slice (shipped)

```
supports_resume: true
auto_resume_on_startup: true
self_assigns_session_id: true
captures_session_id_from_pty: false
captures_session_id_from_cli_list: true
session_assign_args: []
resume_args(id): ["--session", id]
supports_model_override: true
model_args: ["--model", model]          // provider/model
supports_prefill: true
prefill_args: ["--prompt", text]
effort_control: None
requires_attention_hook: false
produces_readable_transcript: false
```

Closest siblings: **Kimi** (`-S` / `--session`, self-assign, model yes) plus **mcode** prefill. OpenCode is Kimi + `--prompt`.

---

## Remaining follow-ups

- Harden capture: plugin `session.created`, `--port` + HTTP, or SQLite. Session-list polling is racy for two Root Nodes in the same directory.
- Attention plugin (`session.idle` / `permission.asked`) so Autopilot can run.
- Transcript reader (`opencode export` JSON) so the archive picker and coordinator rich layer work.
- Remaining CLI: `--auto`, `--agent`, `--fork`, `run --variant`.

---

## Doc drift

- **ADR-0024** — corrected: OpenCode is self-assign + `--session`, not `--session-id` assign.
- **CONTEXT.md** — already listed OpenCode as Model-Configurable; now matches the adapter.
- **Coordinator read-api table** — still groups OpenCode with "no readable transcript". True of the reader, not of OpenCode-the-product (`opencode export`).

---

## Sources

- Live CLI 1.18.3: `opencode --help`, `opencode run --help`, `opencode session --help`, `opencode db path`, `opencode session list --format json -n 3`, and the three `--session` failure cases above
- https://opencode.ai/docs/cli
- https://opencode.ai/docs/server
- https://opencode.ai/docs/plugins
- https://opencode.ai/docs/permissions
- https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/cli/cmd/tui.ts
- https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/cli/tui/validate-session.ts
- https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/cli/cmd/session.ts
- Buildmesh: `adapters/opencode.rs`, `provider/mod.rs`, `capabilities.rs`, `spawn.rs`, `session_capture.rs`, `services/opencode_session.rs`, ADR-0024
