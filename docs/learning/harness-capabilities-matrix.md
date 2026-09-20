---
name: harness-capabilities-matrix
description: Canonical matrix of per-harness capability flags — the truth that the Rust inventory pin test (`agent::capabilities::tests::inventory_matches_research_matrix`) and the TS-side drift gate (`tests/unit/circuits-inspector-capabilities.test.ts`) both gate against.
metadata:
  type: reference
  status: current
  date: 2026-09-20
---

# Harness capabilities matrix

The current truth of every `HarnessCapabilities` field for each adapter
in `src-tauri/src/agent/provider/adapters/`. The Rust inventory pin
(`agent::capabilities::tests::inventory_matches_research_matrix`) is the
authoritative source on the Rust side; the TS mirror at
`src/components/Circuits/harnessCapabilities.ts` is gated against this
matrix by the data-driven
`tests/unit/circuits-inspector-capabilities.test.ts` test. A change to
either side must update the other in the same slice — this file is the
human-readable contract.

For the historical per-harness research notes that landed each
capability decision, see the dated `*-harness-capabilities.md` files in
this directory (`agy-harness-capabilities.md`, `cline-harness-capabilities.md`,
`grok-harness-capabilities.md`, `mcode-harness-capabilities.md`,
`opencode-harness-capabilities.md`). Those files record state at a point
in time and are evidence, not current truth.

## Adapters

The 14 adapters, in the order they appear in
`BUILTIN_HARNESS_IDS` (`src-tauri/src/agent/provider/mod.rs`):

| Harness | Adapter file | Notes |
|---|---|---|
| `anthropic` | `adapters/anthropic.rs` | Claude Code |
| `codex` | `adapters/codex.rs` | |
| `agy` | `adapters/agy.rs` | Antigravity |
| `opencode` | `adapters/opencode.rs` | SQLite transcript reader (`#1296`), attention hook (`#1295`) |
| `grok` | `adapters/grok.rs` | |
| `cursor` | `adapters/cursor.rs` | Hook under `--force` (`#1368` round-2) |
| `kimi` | `adapters/kimi.rs` | Kimi Code |
| `mcode` | `adapters/mcode.rs` | MiniMax Code; manifest-indexed messages.jsonl |
| `dsh` | `adapters/dsh.rs` | DeepSeek Harness; no resume, no model, no prefill |
| `commandcode` | `adapters/commandcode.rs` | Passive turn watcher (`#1481`) |
| `freebuff` | `adapters/freebuff.rs` | |
| `muse` | `adapters/muse.rs` | Meta Muse; JSONL reader (`#1708`), passive watcher (`#1709`), Windows since 1.3.0 |
| `cline` | `adapters/cline.rs` | |
| `terminal` | `adapters/terminal.rs` | Plain shell — every override OFF |

## Resume & startup

`supports_resume` — whether the CLI accepts a resume invocation.
`auto_resume_on_startup` — whether the app auto-resumes suspended
sessions for this harness.

| Harness | `supports_resume` | `auto_resume_on_startup` |
|---|---|---|
| anthropic | true | true |
| codex | true | true |
| agy | true | true |
| opencode | true | true |
| grok | true | true |
| cursor | true | true |
| kimi | true | true |
| mcode | true | true |
| dsh | **false** | **false** |
| commandcode | true | true |
| freebuff | true | true |
| muse | true | true |
| cline | true | true |
| terminal | false | false |

## Attention hook & passive watcher

`requires_attention_hook` — whether the spawn path installs an
attention hook. `attention_capability` — the structured descriptor of
what the hook can deliver (events, launch mode, trust, min version).
`supports_passive_turn_watcher` — whether the harness has a passive
transcript watcher that supplies lifecycle signals when no native
attention hook exists.

| Harness | `requires_attention_hook` | Hook events | Launch mode | Trust | Min version | Passive watcher |
|---|---|---|---|---|---|---|
| anthropic | true | turn_completed, input_required, background_running | skip_permissions | "workspace trust" | none | false |
| codex | true | turn_completed, input_required, permission_requested, background_running | permission_ask | "codex project trust (#1379)" | none | false |
| agy | true | turn_completed, background_running | skip_permissions | "workspace trust" | "1.0.0" | false |
| opencode | true | turn_completed, question_requested, permission_requested | permission_ask | none | none | false |
| grok | true | turn_completed, input_required, permission_requested, question_requested | permission_ask | "global hook dir" | "1.0.5" | false |
| cursor | true | turn_completed, background_running | skip_permissions | none | "1.0.0" | false |
| kimi | false | — | — | — | — | false |
| mcode | false | — | — | — | — | false |
| dsh | false | — | — | — | — | false |
| commandcode | false | — | — | — | — | true |
| freebuff | false | — | — | — | — | false |
| muse | false | — | — | — | — | true |
| cline | false | — | — | — | — | false |
| terminal | false | — | — | — | — | false |

## Transcript reader

`produces_readable_transcript` — whether the harness writes a
transcript the Coordinator read API can parse into a Node Digest's rich
layer (ADR-0008).

| Harness | `produces_readable_transcript` |
|---|---|
| anthropic | true |
| codex | true |
| agy | true |
| opencode | true (`#1296` SQLite reader) |
| grok | true |
| cursor | true |
| kimi | false |
| mcode | true |
| dsh | false |
| commandcode | true |
| freebuff | false |
| muse | true (`#1708` JSONL reader) |
| cline | false (`#1776` deferred) |
| terminal | false |

## Override flags

`supports_model_override`, `supports_effort_override`,
`supports_extra_args`, `supports_prefill`, `is_plain_terminal`.

| Harness | model | effort | extra_args | prefill | plain_terminal |
|---|---|---|---|---|---|
| anthropic | true | true | true | true | false |
| codex | true | true | true | true | false |
| agy | true | true | true | true | false |
| opencode | true | false | true | true | false |
| grok | true | true | true | true | false |
| cursor | true | false | true | true | false |
| kimi | true | false | true | false | false |
| mcode | false | false | true | true | false |
| dsh | **false** | false | true | false | false |
| commandcode | true | true | true | true | false |
| freebuff | false | false | true | true | false |
| muse | true | false | true | true | false |
| cline | true | true | true | true | false |
| terminal | false | false | false | false | true |

## Effort control

`effort_control` — the kind of effort control the harness accepts.
`supports_effort_override` must equal `effort_control != None`; pinned
by the `effort_override_matches_effort_control` test.

| Harness | `effort_control.kind` | Allowed values |
|---|---|---|
| anthropic | closed | `low`, `medium`, `high` |
| codex | inline_config (key `model_reasoning_effort`) | `none`, `low`, `medium`, `high`, `xhigh` |
| agy | closed | `low`, `medium`, `high` |
| opencode | none | — |
| grok | closed | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |
| cursor | none | — |
| kimi | none | — |
| mcode | none | — |
| dsh | none | — |
| commandcode | closed | `low`, `medium`, `high` |
| freebuff | none | — |
| muse | none | — |
| cline | closed | `none`, `low`, `medium`, `high`, `xhigh` |
| terminal | none | — |

## Available platforms

`available_on` — host platforms where this harness runs. **Order
mirrors the adapter's `available_on()` return value** (the same order
is preserved through the IPC wire type and the frontend Inspector).
Most adapters return `[Windows, Linux, Macos]`; Anthropic, Codex,
Command Code, and Terminal return `[Windows, Macos, Linux]`; Codex
returns `[Macos, Windows, Linux]`; Muse returns `[Linux, Macos,
Windows]`.

| Harness | `available_on` |
|---|---|
| anthropic | windows, macos, linux |
| codex | **macos, windows, linux** |
| agy | **windows, linux, macos** |
| opencode | **windows, linux, macos** |
| grok | **windows, linux, macos** |
| cursor | **windows, linux, macos** |
| kimi | **windows, linux, macos** |
| mcode | **windows, linux, macos** |
| dsh | **windows, linux, macos** |
| commandcode | windows, macos, linux |
| freebuff | windows, linux, macos |
| muse | **linux, macos, windows** |
| cline | windows, linux, macos |
| terminal | windows, macos, linux |

## Update protocol

When a capability value changes:

1. Update the Rust adapter in `src-tauri/src/agent/provider/adapters/`.
2. Update the Rust inventory pin
   (`agent::capabilities::tests::inventory_matches_research_matrix`)
   in the same PR.
3. Update `src/components/Circuits/harnessCapabilities.ts` and the
   `EXPECTED_HARNESS_CAPABILITIES` literal in
   `tests/unit/circuits-inspector-capabilities.test.ts` in the same PR.
4. Update this matrix doc in the same PR.
5. The `scripts/check.ps1 all` pipeline runs `cargo test`,
   `npm run test:unit`, `npm run lint`, `npm run check:docs`, and
   `npm run test:docs`; the TS-side data-driven `toEqual` per harness
   trips on any field drift.

A follow-on slice will replace the TS mirror with a Rust-generated
artifact to close the manual-update loop.
