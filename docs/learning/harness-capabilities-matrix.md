---
name: harness-capabilities-matrix
description: Consolidated harness × capability matrix for every Agent Harness Buildmesh supports, derived from the backend capability contract
metadata:
  type: reference
  date: 2026-09-20
---

# Harness capabilities matrix

This is the at-a-glance capability matrix for every Agent Harness Buildmesh
supports. It answers one question per harness: **which capabilities does
Buildmesh actually advertise?**

Every cell is derived from the backend capability contract in
`src-tauri/src/agent/`, not from a vendor's feature list. The matrix describes
what Buildmesh promises for a harness's normal launch mode on a supported
platform. It does not measure whether a hook callback is reliably delivered at
runtime — that evidence lives in
[Harness attention reliability](harness-attention-reliability.md).

For the shorter, user-facing view of the same catalog, see
[Harnesses and capabilities](../user-guide.md#harnesses-and-capabilities).
This page is the detailed reference the user guide summarizes.

## Sources of truth

Update this page from the Rust side. The TypeScript in
`src/components/Circuits/harnessCapabilities.ts` is a *mirror* used by the
Circuit Inspector; it is not authoritative.

| What | Where |
|---|---|
| Capability descriptor (the contract) | [`src-tauri/src/agent/capabilities.rs`](../../src-tauri/src/agent/capabilities.rs) |
| Trait methods and defaults | [`src-tauri/src/agent/provider/mod.rs`](../../src-tauri/src/agent/provider/mod.rs) |
| Per-harness adapter values | [`src-tauri/src/agent/provider/adapters/`](../../src-tauri/src/agent/provider/adapters/) |
| Detection (binary + config dir) | [`src-tauri/src/agent/detection.rs`](../../src-tauri/src/agent/detection.rs) |
| Derived `resumable` flag | [`src-tauri/src/agent/provider_menu.rs`](../../src-tauri/src/agent/provider_menu.rs) |
| Pin test that fails on drift | `inventory_matches_research_matrix` in `capabilities.rs` |

Run `cargo test -p buildmesh agent::capabilities` after an adapter edit, then
`npm run check:docs` before review.

## Harness catalog

Thirteen detected harnesses plus the always-present Terminal give fourteen rows.
"Adapter id" is the stable id matching the database `provider` column.

| Harness | Adapter id | Detection binary | Config dir(s) |
|---|---|---|---|
| Claude Code | `anthropic` | `claude` | `.claude` |
| Codex | `codex` | `codex` | `.codex` |
| Cursor | `cursor` | `cursor-agent` | `.cursor` |
| Antigravity | `agy` | `agy` | `.gemini/antigravity-cli`, `.antigravity`, `.antigravitycli` |
| OpenCode | `opencode` | `opencode` | — |
| Kimi Code | `kimi` | `kimi` | `.kimi` |
| Grok Code | `grok` | `grok` | `.grok` |
| MiniMax Code | `mcode` | `mcode` | `.mcode`, `.minimax-code` |
| DeepSeek Harness | `dsh` | `dsh` | `.dsh`, `.deepseek-harness` |
| Command Code | `commandcode` | `cmdc` (Windows), `cmd` (Unix) | `.commandcode` |
| Freebuff | `freebuff` | `freebuff` | `.config/manicode` |
| Meta Muse | `muse` | `muse` | — |
| Cline | `cline` | `cline` | `.cline` |
| Terminal | `terminal` | — (always present) | — |

Two entries are not harnesses of their own:

- **MiniMax** (the model account) is Claude Code with a different backend, so it
  executes through the `anthropic` adapter. It is distinct from the MiniMax
  Code (`mcode`) harness.
- **Custom Claude-compatible profiles** execute through `anthropic` too.
  The `anthropic` id is also kept as a legacy database value.

## Capability matrix

Legend: ✅ advertised, ❌ not advertised. Column shorthand:

- **Attn hook** — `requires_attention_hook`; spawn installs a native attention hook.
- **Watcher** — `supports_passive_turn_watcher`; a transcript watcher supplies turn signals instead.
- **Transcript** — `produces_readable_transcript`; the transcript reader can parse this harness (Coordinator Node Digest, archive picker).
- **Archive** — the derived `resumable` flag (`resume` **and** `transcript`); surfaces the harness in the archived-node resume picker.
- **Model** — `supports_model_override` (`--model` or equivalent).
- **Effort** — `supports_effort_override` (see [Effort control vocabulary](#effort-control-vocabulary)).
- **Extra args** — `supports_extra_args`; verbatim CLI flags from configuration are forwarded.
- **Prefill** — `supports_prefill`; a seed prompt is delivered at spawn.
- **Plain shell** — `is_plain_terminal`; all LLM paths are skipped.

| Harness | Resume | Auto-resume | Attn hook | Watcher | Transcript | Archive | Model | Effort | Extra args | Prefill | Plain shell | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Claude Code | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | Full hook event set under skip-permissions |
| Codex | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | Hook needs Codex 0.154.0+ and project trust |
| Cursor | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ❌ | Completion and background signals only |
| Antigravity | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | Hook needs CLI 1.0.0+; workspace trust |
| OpenCode | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ❌ | Attention arrives through the project plugin |
| Kimi Code | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ | ✅ | ❌ | ✅ | ❌ | ❌ | Hook needs 0.27.0+; no transcript reader |
| Grok Code | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | Hook validated against 1.0.5 |
| MiniMax Code | ✅ | ✅ | ❌ | ❌ | ✅ | ✅ | ❌ | ❌ | ✅ | ✅ | ❌ | TUI rejects `--model`; transcript reader wired, attention still empty |
| DeepSeek Harness | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ | Capabilities deliberately gated (no validated profile) |
| Command Code | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | Passive watcher replaces a native hook |
| Freebuff | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ✅ | ❌ | No model or effort override |
| Meta Muse | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ❌ | Native on Windows since Muse 1.3.0 |
| Cline | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ | ❌ | Native Provider; attention and reader not shipped |
| Terminal | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | Plain shell; the only `PlainShell` launch mode |

Nine harnesses are archive-resumable (Resume **and** Transcript): Claude Code,
Codex, Cursor, Antigravity, OpenCode, Grok Code, MiniMax Code, Command Code, and
Meta Muse. Kimi Code, Freebuff, and Cline support process resume
but are **not** resumable from the archive picker because they have no
transcript reader. DeepSeek Harness and Terminal do not resume at all.

## Attention hooks

Non-boolean detail for the seven harnesses with a native attention hook
(`attention_capability`). The other seven have no hook wired yet.

| Harness | Launch mode | Lifecycle events | Min. CLI version | Trust requirement |
|---|---|---|---|---|
| Claude Code | Skip permissions | Turn completed, input required, question, permission, background | — | Workspace trust |
| Codex | Permission ask | Turn completed, input required, question, permission, background | 0.154.0 | Codex project trust |
| Cursor | Skip permissions | Turn completed, background | 1.0.0 | — |
| Antigravity | Skip permissions | Turn completed, background | 1.0.0 | Workspace trust |
| OpenCode | Permission ask | Turn completed, question, permission | — | — |
| Kimi Code | Permission ask | Turn completed, input required, question, permission | 0.27.0 | — |
| Grok Code | Permission ask | Turn completed, input required, question, permission | 1.0.5 | Global hook directory |

Launch mode is a real boundary, not a label. Under **Skip permissions** the
harness auto-approves tool calls, so a permission request is impossible by
construction — that is why Cursor and Antigravity signal completion and
background work but never a permission prompt. Under **Permission ask** the
harness can raise a genuine approval signal.

## Effort control vocabulary

| Harness | Control kind | Accepted values |
|---|---|---|
| Claude Code | Closed flag | `low`, `medium`, `high` |
| Codex | Inline config key `model_reasoning_effort` | `none`, `low`, `medium`, `high`, `xhigh` |
| Antigravity | Closed flag | `low`, `medium`, `high` |
| Grok Code | Closed flag | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |
| Command Code | Closed flag | `low`, `medium`, `high` |
| Cline | Closed flag (`--thinking`) | `none`, `low`, `medium`, `high`, `xhigh` |
| All other harnesses | None | — (the resolver drops the effort layer) |

Grok's seven-value list is the superset across models; a given model only
honours the levels its menu advertises.

## Session identity and resume

How each harness's session id is established. "Buildmesh mints" means the spawn
passes the id in (`--session-id` or equivalent); "self-assigns" means the CLI
chooses its own and Buildmesh recovers it.

| Harness | Session id | Recovered from PTY output |
|---|---|---|
| Claude Code | Buildmesh mints | No |
| Codex | Self-assigns | Yes |
| Cursor | Self-assigns | Yes |
| Antigravity | Self-assigns | No (brain-directory poll) |
| OpenCode | Self-assigns | No (SQLite poll) |
| Kimi Code | Self-assigns | Yes |
| Grok Code | Buildmesh mints | No |
| MiniMax Code | Self-assigns | Yes |
| DeepSeek Harness | Buildmesh mints | No |
| Command Code | Self-assigns | No |
| Freebuff | Buildmesh mints | No |
| Meta Muse | Self-assigns | No |
| Cline | Self-assigns | No |
| Terminal | Buildmesh mints | No |

Two other adapter-level behaviours worth knowing when adding support:

- **Native sandbox flag** — Antigravity is the only harness where Buildmesh
  passes a native sandbox argument (`--sandbox`). Meta Muse ships its own OS
  sandbox that Buildmesh disables at launch so the agent's `git`/`gh`
  credentials reach the keyring.
- **Backend env reset** — only the Claude Code adapter clears the
  Claude-compatible backend environment variables before spawn, so a
  Claude-compatible profile cannot leak another harness's backend routing into
  a plain Claude Code node.

## Platform availability

Every harness in the catalog advertises Windows, macOS, and Linux. The
differences are runtime-level, not capability-level:

- Native Windows and WSL installations are detected separately and appear as
  distinct entries.
- Cline is excluded from WSL cross-runtime probing: it is a native
  Windows/macOS/Linux target and its WSL behaviour is undocumented.

## What this matrix does not cover

The capability contract is deliberately narrow. These are **not** modelled per
harness, so this page makes no claim about them:

- MCP servers, user-authored hooks beyond the attention hook, slash commands,
  skills, and subagents.
- Image or vision input, plan mode, and long-term memory.
- Model *selection* UI — only the model-override *pass-through* is modelled.
- Usage meters, which live under `src-tauri/src/services/usage/` and are keyed
  when a fetchable account exists, not per harness capability.

Adding one of these means adding a field to `HarnessCapabilities` first, then a
column here.

## Updating this matrix

1. Change the adapter under `src-tauri/src/agent/provider/adapters/`.
2. Update the pin in `inventory_matches_research_matrix`
   (`src-tauri/src/agent/capabilities.rs`).
3. Refresh the TypeScript mirror in
   `src/components/Circuits/harnessCapabilities.ts` if the Inspector renders it.
4. Update the row here and the summary table in
   [the user guide](../user-guide.md#harnesses-and-capabilities).
5. Run `cargo test -p buildmesh agent::capabilities`, `npm run test:docs`, and
   `npm run check:docs`.

## Related

- [Harness attention reliability audit](harness-attention-reliability.md) — per-harness hook evidence and remaining gaps
- [User guide: harnesses and capabilities](../user-guide.md#harnesses-and-capabilities)
- [Troubleshooting](../troubleshooting.md)
- Per-harness deep dives: [Antigravity](agy-harness-capabilities.md), [Grok Code](grok-harness-capabilities.md), [MiniMax Code](mcode-harness-capabilities.md), [OpenCode](opencode-harness-capabilities.md), [Cline](cline-harness-capabilities.md)
- [Domain vocabulary](../../CONTEXT.md) — Agent Harness vs Model Provider
- [AI context](../knowledge-primer.md)
