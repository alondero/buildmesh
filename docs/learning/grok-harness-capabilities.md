---
name: grok-harness-capabilities
description: Grok Code (Grok Build CLI) capability review against Buildmesh's harness contract — resume, prefill, and unmodeled flags
metadata:
  type: reference
  harness: grok
  grok_version: 1.0.5 (5115b46bc9) [stable]
  date: 2026-08-26
---

# Grok Code harness capabilities vs Buildmesh

Review of what the `grok` binary actually exposes, versus what Buildmesh's Grok adapter advertises and uses. Primary concerns: **session resume** and **prefill**.

**Status (2026-08-26):** items 1 (positional prefill) and 2 (assign `--session-id`) landed on this branch. Remaining follow-ups: #1280 (effort), #1281 (transcripts), #1282 (attention hooks).

## Sources (primary only)

| Source | What it is |
|---|---|
| `grok --help` / `grok --version` on this machine | Live CLI, `grok 1.0.5 (5115b46bc9) [stable]` |
| `~/.grok/docs/user-guide/01-getting-started.md` | Installed Grok Build user guide |
| `~/.grok/docs/user-guide/17-sessions.md` | Session ID, resume, `-s`/`-r`/`-c`, storage layout |
| `~/.grok/docs/user-guide/14-headless-mode.md` | `-p`, `--session-id` create-only, effort vocabulary |
| `~/.grok/docs/user-guide/10-hooks.md` | Notification / Stop / HTTP hooks, Claude-compat paths |
| `https://docs.x.ai/build/cli/reference` | Official CLI reference |
| Live `~/.grok/sessions/<urlencoded-cwd>/<uuid>/` | On-disk session layout on this host |
| `src-tauri/src/agent/provider/adapters/grok.rs` | Current Buildmesh adapter |
| `src-tauri/src/agent/capabilities.rs`, `launch.rs`, `spawn.rs` | Harness contract and spawn path |
| ADR-0024, ADR-0003 | Assign-don't-capture; Buildmesh owns worktrees |

## What Buildmesh wants from a harness

The capability contract (`HarnessCapabilities` / `AgentProvider`, issue #1149 / #1179) is the checklist. For an interactive coding agent Buildmesh cares about:

| Capability | Why it matters |
|---|---|
| `supports_resume` + `resume_args` | Restart a Suspended node on the same conversation |
| `auto_resume_on_startup` | App launch restores suspended nodes without a click |
| Assignable session ID (`self_assigns_session_id = false` + `session_assign_args`) | ADR-0024: mint UUID, write DB *before* launch, pass `--session-id`. PTY capture is the fallback and is flaky |
| `supports_prefill` + `prefill_args` | Issue spawn, handover spawn, Autopilot initial prompt (`SpawnIntent::initial_prompt`) |
| `supports_model_override` | Mesh / app-default `--model` |
| `effort_control` | Mesh / app-default `--effort` (or Codex-style inline config) |
| `requires_attention_hook` | Node Turn → `awaiting_input` + rename (issue #886) |
| `produces_readable_transcript` | Coordinator digest rich layer *and* the archived-node `resumable` picker (`supports_resume && produces_readable_transcript`) |
| Interactive TUI over PTY | Not headless `-p` (exits after one turn) |
| No harness-owned worktree flag | ADR-0003: Buildmesh creates the worktree; never pass `-w` |

Claude Code is the reference shape. Cursor and MiniMax Code (`mcode`) already prove that **positional `[prompt]`** counts as prefill — the trait default `--prefill` is Claude-shaped, adapters override.

## What the Grok adapter advertises

From `adapters/grok.rs` and the inventory pin in `capabilities.rs` **after** prefill + assign-session-id landed on this branch:

| Flag | Adapter value | Notes |
|---|---|---|
| `supports_resume` | `true` | `resume_args` → `--resume <id>` |
| `auto_resume_on_startup` | `true` | |
| `self_assigns_session_id` | `false` | Trait default `--session-id <uuid>` on fresh spawn (ADR-0024) |
| `supports_prefill` | `true` | Trailing positional `[PROMPT]`; not `--prefill`, not `-p` |
| `supports_model_override` | `true` | `--model <id>` |
| `effort_control` | `None` | Grok CLI has `--effort`; unwired — #1280 |
| `requires_attention_hook` | `false` | Grok has Notification/Stop/HTTP; unwired — #1282 |
| `produces_readable_transcript` | `false` | On-disk ACP JSONL exists; unwired — #1281. Archived picker `resumable: false` |
| Shell | `WindowsShell::Direct` | Correct — native binary, not a `.cmd` shim |
| Launch mode | Interactive TUI | Correct — `#914` verified ConPTY; `-p` is unused |

## Resume — Grok has more than we use

### What already matches

Grok's resume CLI is the same shape Buildmesh already emits:

```
-r, --resume [<SESSION_ID_OR_TITLE>]
        Resume a session by ID or title, or the most recent if omitted.
```

(`grok --help`; also `17-sessions.md` "From the Command Line".)

`GrokAdapter::resume_args` emits `--resume <id>`. Auto-resume on startup is on. If `cli_session_id` is stored, Suspended Grok nodes *can* come back.

Resume is **cwd-scoped**. Sessions live at `~/.grok/sessions/<urlencoded-cwd>/<session-id>/`. A UUID that belongs to another working directory will not resume in this one. That is compatible with Buildmesh: we always spawn in the node's working directory.

`--continue` / `-c` continues the most recent session *in the current directory*. Buildmesh does not use it — same choice as Kimi (`kimi.rs`: "the bare `-c` form is intentionally not modelled; auto-resume always passes the captured session id"). Keep that: an explicit ID is the only safe orchestrator path.

### Gap 1 (landed): assign `--session-id` instead of PTY capture

Installed docs (`17-sessions.md`):

> Sessions are identified by a unique session ID (a **UUIDv7 when Grok generates it; a client may supply its own ID with `-s`**)

> Use `-s`/`--session-id` only to **create** a new session with a **UUID** (errors if the value is not a UUID, or if that ID already has a session under the target session directory). It does **not** resume an existing session — that was the old hidden upsert behavior; use `-r`/`-c` instead.

Live `--help` for `-s`:

> Use a specific session UUID for a **new** conversation (must be a valid UUID and must not already exist under the target session directory). With `--resume`/`--continue`, only valid together with `--fork-session`. Does **not** resume existing sessions.

This is Claude Code's anti-overwrite model, which is exactly ADR-0024:

1. Fresh spawn: mint UUID, write `agent_nodes.cli_session_id`, pass `--session-id <uuid>` (`SessionIdMode::Assign`).
2. Resume: pass `--resume <uuid>` (`SessionIdMode::Resume`).
3. Do **not** sniff the ID out of PTY output.

**Landed:** `self_assigns_session_id` is no longer overridden (trait default `false`); empty `session_assign_args` was dropped so the trait default `--session-id <uuid>` is used. Fresh spawns take `SessionIdMode::Assign`. Existing nodes with a captured v7 ID still resume via `--resume`. Nodes with NULL `cli_session_id` still cannot auto-resume.

Capture only matches a UUID that is **labelled** `session:` / `session id:` / `conversation:` / `conversation id:`. A bare UUID in TUI output is ignored (pinned: `ignores_uuid_without_label`). That path is now unused for Grok.

Grok-generated IDs on this host are UUIDv7 (`01a0400a-6ac5-7d90-a1a6-b5397ff81d62` in `summary.json`). Buildmesh mints `Uuid::new_v4()`, which is a valid UUID and is what `-s` requires. Mixing v4 (Buildmesh) and v7 (Grok-native) is fine because we own the ID on fresh spawn.

### Gap 2 (medium): archived-node picker hides Grok even though `--resume` works

`provider_menu.rs`:

```text
resumable = adapter.supports_resume() && adapter.produces_readable_transcript()
```

Grok is `supports_resume: true` and `produces_readable_transcript: false`, so the archived-session `▾` picker treats it like OpenCode. Startup auto-resume of *Buildmesh-owned* Suspended nodes is a different path and does not use this flag — but "resume an on-disk Grok conversation we didn't spawn" is unavailable.

Grok *does* write a transcript. Layout (docs + this host):

```
~/.grok/sessions/<urlencoded-cwd>/<session-id>/
  summary.json        # id, cwd, title, model, timestamps, reasoning_effort
  updates.jsonl       # ACP session-update stream (authoritative for /resume)
  chat_history.jsonl  # raw model messages
  rewind_points.jsonl
  signals.json
```

CWD encoding is **percent-encoding of the Windows path** (`F%3A%5Csrc%5Cbuildmesh%5C...`), not Claude Code's alphanumeric-dash encoding. `TranscriptFormat` today is `ClaudeCode | Cursor | Codex` — a Grok variant would be a new path resolver + ACP-JSONL parser, analogous to the unwired Kimi follow-up (#911). Until that exists, leaving `produces_readable_transcript = false` is honest; flipping it without a reader would lie to the coordinator digest.

`grok export <session-id>` dumps Markdown. `grok sessions list/search` is cwd-scoped. Neither is wired.

### Other resume-adjacent flags we should *not* pass

| Flag | What it does | Why Buildmesh should ignore it |
|---|---|---|
| `--fork-session` | Resume into a new session ID | Orchestrator owns identity; forking would desync `cli_session_id` |
| `--restore-code` | Check out the original session's repo snapshot on resume | Fights ADR-0003 / ADR-0024 worktree reuse |
| `-w, --worktree` | Grok creates its own git worktree | Same; `grok --help` even warns `-w "prompt"` swallows the positional prompt as a worktree name |
| `grok import` | Import Claude Code sessions | Interesting for a future "adopt foreign transcript" feature; not spawn |

## Prefill — positional `[PROMPT]` (landed)

### The interactive-TUI initial prompt is prefill

`grok --help`:

```
Arguments:
  [PROMPT]
          Initial prompt for the interactive session, e.g. `grok "fix the bug"`
```

Getting Started (`01-getting-started.md`):

```bash
# Launch the interactive TUI and submit an initial prompt as the first turn
grok "fix the failing auth test and run it"
```

That is the same contract Cursor and MiniMax Code already implement: trailing positional, no `--prefill` flag.

- Cursor: `prefill_args` → `vec![text]`, `supports_prefill = true`
- mcode: same
- Grok: same (landed). Before this branch, `supports_prefill = false` dropped issue/handover/Autopilot prefills with a warn.

There is **no** `--prefill` flag. Claude-compat aliases documented on the official CLI reference are `--allowedTools`, `--disallowedTools`, `--append-system-prompt`, `--system-prompt`, `--dangerously-skip-permissions`. Emitting the trait-default `--prefill` would be rejected. The adapter must override `prefill_args` the Cursor/mcode way.

### Do not confuse this with headless `-p`

| Form | Mode | Buildmesh use |
|---|---|---|
| `grok "prompt"` (positional) | Interactive TUI, first turn submitted | **Yes — this is prefill** |
| `grok -p/--single "prompt"` | Headless: run, print, **exit** | No — kills the PTY node |
| `--prompt-file` / `--prompt-json` | Also trigger headless | No |
| `--verbatim` | Send the prompt exactly as given | Optional later; newline normalisation already lives in `launch.rs` |

The inventory pin now treats Kimi as the remaining interactive TUI without prefill. Grok sits with Cursor / mcode.

`--worktree` is the only documented footgun: `grok -w "refactor module X"` treats the string as a worktree name. Buildmesh must not pass `-w` (ADR-0003), so a trailing positional prompt is unambiguous.

### Prefill + resume together

Headless docs show `grok -p "continue" --resume <id>`. Interactive equivalent is `grok --resume <id> "follow-up prompt"`. Buildmesh's launch helper appends prefill after resume args (`default_prepare`: session flags, then model/effort, then prefill). That argument order is `grok --resume <id> --model … <prompt>`, which matches the CLI. Whether the TUI submits that prompt as a new turn on resume is not separately documented for interactive mode; headless clearly does. Worth a one-line PTY smoke test before relying on handover-into-a-resumed-Grok-node.

## Effort — Grok has a closed vocabulary; we advertise `None`

`grok --help`:

```
--reasoning-effort <EFFORT>
        Reasoning effort for reasoning models
        [aliases: --effort]
```

Headless docs (also "Works in TUI and headless"):

> Canonical levels: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` (each a distinct tier; a model only accepts the levels its menu advertises).

A live `summary.json` on this host recorded `"reasoning_effort": "high"`.

Compared with existing `EffortControlKind::Closed` vocabularies:

| Harness | Vocabulary |
|---|---|
| Claude Code | `low`, `medium`, `high` |
| Codex | `none`, `low`, `medium`, `high`, `xhigh` |
| **Grok (unwired)** | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |

Trait default `effort_args` already emits `--effort <level>`, which is a documented Grok alias. Wiring this is: override `effort_control()` with `Closed { allowed: GROK_EFFORT_ALLOWED }`. The inventory pin currently expects Grok `None`; that test and the `#1179` coherence table's `effort_value` match arm (`_ => "high"`) would need a Grok case.

Not a resume/prefill blocker, but it is a capability Grok has that Buildmesh's resolver will currently drop.

## Attention hooks — Grok speaks Claude's events, including HTTP

Buildmesh's Node Turn signal is a catch-all `Notification` + `Stop` POST to `/api/attention/$BUILDMESH_SESSION_ID` (issue #886). Grok's hook surface (`10-hooks.md`) includes:

| Event | Relevance |
|---|---|
| `Notification` (`idle_prompt`, `permission_prompt`, `task_complete`, …) | Same yield types CONTEXT.md maps to Node Turn |
| `Stop` | Turn ended; can block-stop like Claude |
| HTTP handler `{ "type": "http", "url": "…" }` | Native POST; no curl wrapper |

Grok **reads Claude hook files by default**:

- Global: `~/.claude/settings.json` and `settings.local.json` (always trusted)
- Project: `<project>/.claude/settings.json` and `settings.local.json` (**requires folder trust**)

Mesh creation already writes `.claude/settings.local.json` via `inject_attention_hook`. A Grok node in that mesh *might* fire those hooks if the user has `/hooks-trust` / `--trust` on the folder. That is accidental, not owned: `requires_attention_hook` is still `false`, so a Grok-only spawn does not inject anything, and untrusted project hooks are "silently skipped".

A first-class Grok attention path would write `.grok/hooks/*.json` (or `~/.grok/hooks/`, which is always trusted) with `Notification` + `Stop` HTTP handlers. `--trust` is **not** in `grok --help`; docs say it exists for launch. Confirm before baking it into the recipe.

## Other Grok capabilities outside the current contract

These are real, first-party, and unused. None of them are required for resume/prefill.

| Capability | Flag / path | Buildmesh analogue |
|---|---|---|
| ACP stdio | `grok agent stdio` | Alternative to PTY; session/new + session/load. Out of scope while TUI-in-PTY is the product |
| Native sandbox | `--sandbox <profile>` | Mesh Sandbox is a different mechanism (Seatbelt / restricted token). Do not double-sandbox without a ticket |
| Always-approve | `--always-approve` / `--yolo` | Claude-family uses cwrap / AGY uses `--dangerously-skip-permissions`. Not in the capability struct |
| Extra rules | `--rules <text>` | Overlaps AGENTS.md injection; skip |
| Model | `-m` / `--model` | Already wired |
| Sessions CLI | `grok sessions list\|search\|delete` | Discovery / archived picker |
| Export | `grok export <id>` | Digest fallback |
| Import Claude | `grok import` | Cross-harness resume |
| `--no-alt-screen` | Inline TUI, no alternate screen | Possibly kinder to xterm.js; unverified |
| `--no-auto-update` | Skip update checks | Headless/CI hygiene; optional |

## Capability matrix (Grok vs the contract)

| Contract field | Adapter after this branch | Grok CLI 1.0.5 | Action |
|---|---|---|---|
| Resume by ID | `--resume <id>` | Yes | Keep |
| Auto-resume on startup | true | Yes, if we have the ID | Keep |
| Assign session ID | `--session-id <uuid>` (landed) | `-s/--session-id <UUID>` create-only | Done (ADR-0024) |
| Continue most-recent | unused | `-c` cwd-scoped | Keep unused |
| Prefill | positional `[PROMPT]` (landed) | positional on interactive TUI | Done |
| Model override | `--model` | Yes | Keep |
| Effort | **None** | **`--effort` / `--reasoning-effort` closed vocab** | #1280 |
| Attention hook | false | Notification + Stop + HTTP; Claude-compat files | #1282 |
| Readable transcript | false | `~/.grok/sessions/<enc-cwd>/<id>/updates.jsonl` | #1281 |
| Worktree flag | not passed | `-w` exists | Keep not passing |

## Recommended follow-ups (resume & prefill first)

1. **Prefill — landed.** `supports_prefill = true`, positional `prefill_args`.
2. **Assign session IDs — landed.** Trait default `--session-id`; resume still `--resume <id>`.
3. **Effort — #1280.** `EffortControlKind::Closed` with `none|minimal|low|medium|high|xhigh|max`. Trait-default `--effort` already matches.
4. **Transcript reader — #1281.** New `TranscriptFormat`; unblocks archived-picker `resumable`.
5. **Attention hooks — #1282.** Grok-native Notification + Stop HTTP.
6. **Not in this pass:** ACP, `--fork-session`, `--restore-code`, `-w`, `-p`.

## Open questions (cannot answer from docs alone)

1. Does the interactive TUI print a labelled `session id: <uuid>` on startup? Irrelevant for new Grok nodes (we assign); still matters for any other self-assigning harness.
2. Does `grok --resume <id> "follow-up"` submit the positional prompt as a new turn in the TUI, or only restore conversation? Headless `-p` + `-r` does submit.
3. Does `-s/--session-id` work on the interactive TUI (global option — docs imply yes) or only with `-p`? `--help` lists it as a top-level option, not headless-only.
4. Folder-trust: will a mesh's existing `.claude/settings.local.json` attention hook fire for Grok without `--trust`? Docs say project Claude hooks require trust and are otherwise silently skipped.
