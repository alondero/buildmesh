---
name: harness-capabilities-matrix
description: Consolidated harness × capability matrix for every Agent Harness Buildmesh supports, derived from the backend capability contract
metadata:
  type: reference
  date: 2026-09-20
---

# Harness capabilities matrix

> Capability values are generated from `src-tauri/src/agent/harness_catalog.rs`
> into `src/types/generated/HarnessCapabilitiesTable.ts`. Re-emit with
> `cargo test` (cwd `src-tauri/`) and update this matrix so every label still
> appears as a `| <label> |` row. See
> [ADR-0037](../adr/0037-generated-harness-capabilities-catalog.md).

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
| MiniMax Code | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ❌ | ❌ | ✅ | ✅ | ❌ | TUI rejects `--model`; attention hook validated on 0.4.12 (`Stop` only) |
| DeepSeek Harness | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ | ❌ | Capabilities deliberately gated (no validated profile) |
| Command Code | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | Passive watcher replaces a native hook |
| Freebuff | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | ✅ | ❌ | No model or effort override |
| Meta Muse | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ❌ | Native on Windows since Muse 1.3.0; workspace trust pre-provisioned (#1706) |
| Cline | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | Native Provider; hook delivers turn completion only (no clean-exit dispatch); reader over `<id>.messages.json` (#1776) |
| Terminal | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ | Plain shell; the only `PlainShell` launch mode |

Ten harnesses are archive-resumable (Resume **and** Transcript): Claude Code,
Codex, Cursor, Antigravity, OpenCode, Grok Code, MiniMax Code, Command Code, Meta
Muse, and Cline. Kimi Code and Freebuff support process resume
but are **not** resumable from the archive picker because they have no
transcript reader. DeepSeek Harness and Terminal do not resume at all.

## Background inference

`background_inference` is nullable. A non-null descriptor is generated from
the adapter's `background_recipe`, which must fulfill all of these requirements:

- Accept one supplied prompt without a terminal, using its declared input transport.
- Return a final assistant answer through a declared channel that excludes progress,
  reasoning, and tool results.
- Exit after the request without requiring approval, questions, or terminal input.
- Honor the selected native login or an explicitly supported provider route.

The caller supplies an isolated temporary directory and a 30-second deadline;
naming retains cancellation cleanup, and classifiers retain bounded pipe readers
and process-tree cleanup. Interactive prefill and attention hooks alone do not
establish background support. This capability concerns the inference backend;
the node being named still needs a turn signal to trigger naming.

| Harness | Prompt input | Final answer | Provider routing |
|---|---|---|---|
| Claude Code | stdin (`--print`) | text stdout | Native or configured route |
| Codex | stdin (`exec -`) | `--output-last-message` file | Native only |
| OpenCode | stdin (`run --format json`) | final text event | Native only |
| Kimi Code | `--prompt` argument | final assistant JSON message | Native only |
| Grok Code | `--prompt-file` | plain stdout | Native only |
| Antigravity | `--print` argument | text stdout | Native only |
| Command Code | stdin (`--print`) | JSON result event | Native only |
| MiniMax Code | stdin (`exec --input -`) | `--output-last-message` file | Native only |

Cursor, Cline, Freebuff, Meta Muse, DeepSeek Harness, and Terminal currently
advertise no background recipe. This does not imply their CLI lacks a headless
mode: Buildmesh requires a validated recipe and final-answer extractor before
offering one. New adapters inherit unavailable background support by default.

Saved model and effort settings use the existing capability mask. Extra CLI
arguments, WSL, and Windows Interop selections are refused for background work;
unsupported provider routing is refused without falling back to another login.
Both the auto-naming and Circuit-classifier settings pickers consume the same
descriptor, including custom profiles backed by a supported adapter.
Argument transports (Kimi Code and Antigravity) advertise a 16,000-byte prompt
limit to stay below Windows command-line limits; larger reports fail explicitly.
Stdin and prompt-file transports do not have this argument-size restriction.

Sources for the recipes: installed CLI help (Claude Code 2.1.286, Codex 0.159.3,
OpenCode 1.18.3, Kimi Code 0.27.0, Grok Code 1.0.44, Command Code 1.72.4),
[Claude Code CLI reference](https://code.claude.com/docs/en/cli-reference),
[OpenCode run implementation](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/cli/cmd/run.ts),
and [Kimi Code command reference](https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-command.html).

## Attention hooks

Non-boolean detail for the nine harnesses with a native attention hook
(`attention_capability`). The other five have no hook wired yet.

| Harness | Launch mode | Lifecycle events | Min. CLI version | Trust requirement |
|---|---|---|---|---|
| Claude Code | Skip permissions | Turn completed, input required, question, permission, background | — | Workspace trust |
| Codex | Permission ask | Turn completed, input required, question, permission, background | 0.154.0 | Codex project trust |
| Cursor | Skip permissions | Turn completed, background | 1.0.0 | — |
| Antigravity | Skip permissions | Turn completed, background | 1.0.0 | Workspace trust |
| OpenCode | Permission ask | Turn completed, question, permission | — | — |
| Kimi Code | Permission ask | Turn completed, input required, question, permission | 0.27.0 | — |
| Grok Code | Permission ask | Turn completed, input required, question, permission | 1.0.5 | Global hook directory |
| MiniMax Code | Skip permissions (pinned: `permissionMode: bypassPermissions`) | Turn completed | 0.4.12 | — |
| Cline | Skip permissions | Turn completed | 3.0.62 | — |

Launch mode is a real boundary, not a label. Under **Skip permissions** the
harness auto-approves tool calls, so a permission request is impossible by
construction — that is why Cursor and Antigravity signal completion and
background work but never a permission prompt. Under **Permission ask** the
harness can raise a genuine approval signal.

## Review-circuit eligibility

Two columns above — **Attn hook** and **Watcher** — decide whether a harness can
take part in a review Circuit at all, either as the reviewed source agent or as
the reviewer agent node.

The reason is structural, not stylistic. A Circuit gate that waits on an agent
(`AwaitAgentTurn`, `ReviewVerdict`) parks in `Running` until that agent's status
reaches `awaiting_input` / `ready` / `completed`, and those statuses only ever
arrive from the attention/lifecycle path. A harness carrying **neither** signal
can never satisfy a wait, so the run does not fail fast — it parks until the
watchdog budget expires. **Plain shell** is excluded for the same reason it is
excluded everywhere else: there is no agent to yield at all.

This is the harness half of the backend Autopilot gate
(`src-tauri/src/autopilot/compatibility.rs`, reason `MissingAttentionHook` /
`PlainTerminal`), so the same predicate that decides whether a Mesh may be
Autopilot-managed also decides who may review. The frontend mirror is
`blocksReviewCircuit` in `src/components/Circuits/harnessCapabilities.ts`. It
gates the Agent Node title-bar review control and the two interactive reviewer
provider pickers (the Start Review dialog and Settings → Providers).

It deliberately does **not** gate the Circuit Inspector's provider field on a
reviewer agent node. Authored Circuits carry their reviewer provider in the
graph and are intentionally exempt from the Autopilot compatibility gate
(`services/circuit_worker/spawn.rs`), so an author may still build a graph whose
reviewer harness has no turn signal — such a run advances only if nothing waits
on that agent. The gate also passes any provider id it cannot resolve, because
the column holds user-defined harness profile ids that resolve to a real
executor at the spawn seam.

| Harness | Turn signal | Review circuit |
|---|---|---|
| Claude Code | Attn hook | ✅ |
| Codex | Attn hook | ✅ |
| Cursor | Attn hook | ✅ |
| Antigravity | Attn hook | ✅ |
| OpenCode | Attn hook | ✅ |
| Kimi Code | Attn hook | ✅ |
| Grok Code | Attn hook | ✅ |
| MiniMax Code | Attn hook | ✅ |
| Cline | Attn hook | ✅ |
| Command Code | Watcher | ✅ |
| Meta Muse | Watcher | ✅ |
| DeepSeek Harness | None | ❌ |
| Freebuff | None | ❌ |
| Terminal | None (plain shell) | ❌ |

Eligibility is necessary, not sufficient, and two of the ✅ rows are weaker than
the rest:

- **Kimi Code** advertises a hook, but its CLI hook contract validation is still
  open (#1554, blocking #1369). Until that lands the gate admits Kimi on an
  unvalidated signal.
- **Kimi Code** has no transcript reader, so its reviewer report comes from the
  PTY tail rather than a parsed transcript. `produces_readable_transcript` is a
  quality-and-recovery factor, not a gate: the transcript is the *preferred*
  report source in the circuit worker, and it is what re-reads a turn that
  completed while capture was offline.

The review **verdict** remains separate from lifecycle readiness. Circuit dispatch
requests a versioned final review result; a valid result routes without a second
model call after evidence binding. Free-form reports use the Mesh's configured
Autopilot classifier, with a deterministic verdict fallback for clean yielded
reports when that backend is unavailable. A clean lifecycle alone does not approve
a review, and an explicit result does not prove native owned-work completion.
See [node review circuits](../development/agent-node-circuits.md).

## Effort control vocabulary

| Harness | Control kind | Accepted values |
|---|---|---|
| Claude Code | Closed flag | `low`, `medium`, `high`, `xhigh`, `max` |
| Codex | Inline config key `model_reasoning_effort` | `none`, `low`, `medium`, `high`, `xhigh` |
| Antigravity | Closed flag | `low`, `medium`, `high` |
| Grok Code | Closed flag | `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` |
| Command Code | Closed flag | `low`, `medium`, `high` |
| Cline | Closed flag (`--thinking`) | `none`, `low`, `medium`, `high`, `xhigh` |
| All other harnesses | None | — (the resolver drops the effort layer) |

Claude Code and Grok expose the union of their model-specific effort levels;
availability depends on the selected model.

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
- Per-harness deep dives: [Antigravity](agy-harness-capabilities.md), [Claude Code](claude-code-harness-capabilities.md), [Grok Code](grok-harness-capabilities.md), [MiniMax Code](mcode-harness-capabilities.md), [OpenCode](opencode-harness-capabilities.md), [Cline](cline-harness-capabilities.md), [Meta Muse](muse-harness-capabilities.md)
- [Domain vocabulary](../../CONTEXT.md) — Agent Harness vs Model Provider
- [AI context](../knowledge-primer.md)
