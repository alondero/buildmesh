---
name: review
description: Independently review a Buildmesh branch or working change against its spec and engineering contract; follow up on batched review repairs before final approval.
---

# Review a Buildmesh change

Read the [engineering contract](../../../docs/agents/engineering.md) and
[review and repair policy](../../../docs/agents/development-harness.md#review-and-repair).
The implementer requests an independent reviewer; the implementer cannot
approve their own work. A review request authorizes inspection and focused
read-only checks. Publish review comments only when the user authorizes them.

## Establish the change

Use the task's immutable base or the supplied comparison ref. Record the
current HEAD and working-tree state. Inspect the complete change since that
base, including staged, unstaged, deleted and untracked files; an isolated
repair diff is insufficient for the initial review. Read the originating spec,
observable acceptance criteria and available verification evidence. State
missing evidence and mock/runtime limits explicitly.

## Initial pass

Review the whole change against both the spec and documented standards.
Report all substantiated findings in one batch, with file/location, severity,
the violated requirement or invariant, and a concrete failure scenario or
missing evidence. Separate required corrections from optional suggestions.
Complete the pass before reporting; a single finding is not a stopping point.
Exhaustiveness is best effort, since subsequent repairs may introduce defects.

## Follow-up pass

Use the previous reviewed revision or captured diff, prior findings and repair
diff to retain context. Check every required finding's resolution through code
and evidence, then inspect the repairs for new defects and interactions with
the rest of the change. Identify whether a new finding was introduced by the
repair or missed in the earlier pass. Expand to a complete review when history
is unavailable or scope changes. Report all remaining findings together.

Fast checks plus relevant behavioral regressions are sufficient to begin a
follow-up review. Require broader checks during repair when the affected
boundary warrants them; full verification is required for final handoff.
Keep unresolved failures and evidence gaps visible until they are resolved.

## Verdict and handoff

Return APPROVE, REQUEST_CHANGES or BLOCKED, the reviewed revision and any
working-tree changes, findings, and a summary of evidence and its limits.
REQUEST_CHANGES lists required corrections; optional suggestions are separate.
BLOCKED names the missing prerequisite. APPROVE explicitly endorses the
complete current change and has no unresolved required findings.

Semantic approval can precede final verification, but completion requires a
PASS from scope-complete `npm run verify`, acceptance evidence and independent
APPROVE recorded for that same final tree. Preserve those completion guards.
Any later source edit requires refreshed evidence and approval; approval of an
older diff does not cover later unreviewed edits. The implementer records the
reviewer's verdict with `harness update` and calls `harness finish` only when
all completion evidence is current. Reviewers do not finish the author's task.
