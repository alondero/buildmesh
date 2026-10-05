---
name: claude-code-harness-capabilities
description: Claude Code native hook contract as Buildmesh consumes it, including the authoritative submission-correlation mechanism for Circuit lifecycle evidence
metadata:
  type: reference
  harness: anthropic
  min_version: 2.1.196
  tested_version: 2.1.283
  date: 2026-09-27
---

# Claude Code harness vs Buildmesh capability contract

What Buildmesh can and cannot prove about a Claude Code turn, and the exact
mechanism that ties a submitted Buildmesh input to Claude's lifecycle
observations (issue #1898).

Sources: the official [hooks reference](https://code.claude.com/docs/en/hooks)
for the native payload contract, the installed CLI version recorded below for
the runtime inventory, and the owning modules
(`services/circuit_worker/native_hooks.rs`,
`db/circuit/evidence.rs`, `http/routes/attention.rs`).

## Status

| Fact | Native source | Status |
|---|---|---|
| Session identity | `session_id`, fenced against the node's stored session | Validated |
| Session incarnation | Buildmesh's own process generation, not a Claude field | Validated |
| Per-turn token | `prompt_id` (Claude Code v2.1.196+) | Contract documented; **live delivery unverified** |
| Submitted prompt text | `UserPromptSubmit.prompt` | Contract documented; **live delivery unverified** |
| Submission correlation | prompt-echo digest + submission ordering, below | Implemented and fixture-tested; **live delivery unverified** |
| Child/background registry | none in the hook payload | Unavailable |
| Inline final report | `Stop.last_assistant_message` | Present, secret-scrubbed |
| Human waits | `PermissionRequest` / `PreToolUse` / `PostToolUse` `tool_use_id` | Validated, exact-id only |

Installed inventory for this environment: Claude Code 2.1.283 on Windows, which
is above the `prompt_id` floor. The user has no Claude account, so no live
request/reply smoke has been recorded. Everything below the "Status" table is
therefore contract-and-fixture evidence, not a runtime pass.

## 1. The native fields, and what each one can prove

`UserPromptSubmit` is the only hook event that carries both halves of a
submission acknowledgement:

```json
{
  "session_id": "abc123",
  "prompt_id": "550e8400-e29b-41d4-a716-446655440000",
  "transcript_path": "/Users/.../00893aaf-….jsonl",
  "cwd": "/Users/...",
  "permission_mode": "default",
  "hook_event_name": "UserPromptSubmit",
  "prompt": "Write a function to calculate the factorial of a number"
}
```

- `prompt` is the text the user submitted. Pasted content that collapsed to a
  `[Pasted text #N]` placeholder arrives expanded in place, so the field is
  the text Buildmesh wrote — but the *documented* field, not a Buildmesh
  guarantee. A CLI that transformed it would simply fail to match, and the
  receipt degrades rather than mis-binding.
- `prompt_id` is the UUID naming the prompt currently being processed. It
  appears on every hook event, not just `UserPromptSubmit`, which is what lets
  `Stop` name the same turn. It is **absent** before the first user input and
  on versions before v2.1.196.
- `Stop` carries `prompt_id` and `last_assistant_message` but **not** `prompt`.
  It can therefore identify *which turn* ended, and never *which input* that
  turn was — the missing half is the whole problem this issue addresses.
- `transcript_path` is explicitly documented as possibly lagging the
  in-memory conversation, so it is never used as a submission proof.

A turn id on its own authorizes nothing. `NativeHook::parse` therefore
retains `prompt_digest` only when the event is `UserPromptSubmit`, the
provider is the Claude family, **and** a turn token is present — a
half-populated acknowledgement candidate is not stored at all.

## 2. The correlation mechanism

Buildmesh owns the submission side: it knows exactly what text it wrote into
the PTY and when. Claude owns the acknowledgement side: it reports the text it
received and the turn it opened. The binding is the intersection.

1. **Record before writing.** When a Circuit `InjectPty` effect claims a
   prompt, `record_prompt_submission` appends a `prompt_submitted` history row
   holding the agent node, a per-agent ordinal (`submission_seq`), and the
   SHA-256 of the prompt text. The prompt text itself is never persisted.
   This happens *before* the PTY write, because Claude can echo the prompt
   within milliseconds of Enter and there must be no window in which Buildmesh
   has written but has no record to match against.

2. **Earn the binding.** A `UserPromptSubmit` receipt is bound to that
   submission only when all four hold, inside one (run, step, attempt, agent)
   scope:
   - the receipt carries a current input stamp, so Buildmesh can still say
     which submission is live;
   - the harness's prompt digest equals a recorded submission's digest — the
     harness received byte-for-byte the text Buildmesh wrote;
   - that submission is still the **newest** for the agent node;
   - no *other* turn has already claimed that same submission.

3. **Inherit it.** A later `Stop` names the same `prompt_id`, so it reads the
   bound stamp *and* the submission ordinal back from the persisted turn-start
   receipt. Both travel together: the terminal receipt records which submission
   it completed, so the ledger answers "which input ended here?" without
   re-deriving it from the turn start. That receipt then replays with
   `submission_correlated = true` and the observation is authoritative under
   the existing freshness fences.

### Why each hazard is closed

| Hazard | Guard |
|---|---|
| **Delayed** — a start hook for turn A lands after Buildmesh submitted B | Rule 3. A's digest resolves to A's *older* ordinal, which is no longer the newest, so the binding is refused. A's `Stop` therefore finds no bound turn start and stays uncorrelated. |
| **Duplicate** — the same delivery arrives twice, or two turns report the same text | Receipt dedup is by payload hash, and rule 4 refuses a second turn claiming an already-claimed submission. A redelivery of the *same* turn re-reads its own binding and is idempotent. |
| **Prior-turn** — a `Stop` arrives after a newer submission | The bound stamp no longer equals the live input stamp, so `normalize` marks the receipt stale and every fact in it is rejected. |
| **Cross-run / cross-step** — a hook lands on a run that did not submit | The submission lookup is scoped to the same (run, step, attempt, agent) the receipt is being recorded against, and a submission recorded for another step is invisible to it. |

## 3. The explicit unavailable path

Every refusal records the receipt, replays it, and presents it in run history
and the Probe. It is simply marked reduced confidence, which cannot complete a
step or satisfy a gate:

- no `prompt_id` (Claude Code before v2.1.196, or before first user input);
- no `prompt`, an empty `prompt`, or a `prompt` the harness transformed so the
  digest no longer matches;
- a superseded submission, or an ambiguous claim by a second turn;
- a session-generation change between the turn start and the receipt.

This is why `observer_policy::for_provider("anthropic")` names the mechanism
*and* says live delivery is unverified. The policy describes the wired
capability; it is not a claim that any run exercised it.

## 4. What correlation does not buy

Binding a turn to a submission establishes the lifecycle fact, not task
completion. Claude Code's `Stop` payload carries no child or background
registry, so the receipt that ends a turn also reports
`OwnershipUnavailable`, and `lifecycle_verified()` stays false. A correlated
Claude `Stop` upgrades the turn's own facts from reduced confidence to
accepted; it does not complete a step. Owned-work coverage remains the open
gap, as does live delivery.

## 5. Boundaries

- The binding is recorded for Circuit `InjectPty` submissions. A
  `ContinueAgentTurn` continuation bumps the input stamp without recording a
  submission, so that turn's receipts are uncorrelated — conservative, never
  mis-binding.
- `submission_seq` is allocated per agent node across all runs, because the
  fence that matters ("has Buildmesh submitted to this terminal again") is a
  property of the PTY, not of one run.
- Session and incarnation fences are unchanged and still apply first: a hook
  from a previous process generation is dropped at the attention route before
  any of this runs.

## Related

- [Circuit reliability acceptance](../archive/2026-09/circuit-reliability-acceptance.md)
- [Circuit session observation and autonomous supervision](../development/circuit-session-observation.md)
- [Harness attention reliability audit](harness-attention-reliability.md)
- [Harness capabilities matrix](harness-capabilities-matrix.md)
- [Antigravity Circuit contract](agy-harness-capabilities.md) — the contrasting
  harness with no turn token at all
