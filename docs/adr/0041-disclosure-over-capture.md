# 41. Disclosure over capture: superseding ADR-0029's Probe Context Pin mandate

## Status

Accepted (2026-10-05). Supersedes the pinning mandate of
[ADR-0029](0029-probe-context-lenses.md) — specifically its "Following
selection and pinning" requirement that "Every non-Host destination is
pinnable when it has a target" — and the consequences that depended on it.
ADR-0029 itself is unedited and remains the record of the ownership lens,
baseline, and mixed-ownership decisions, which are unchanged (see below).
This record reverses one mandate; it does not reopen the others.

## Context

ADR-0029 diagnosed the real problem precisely: "Selection is also live UI
state. A Mesh or Agent Node can change while a stateful view remains
mounted, which makes an action's target ambiguous unless the shell names
the target or the user can capture it."

That sentence offers exactly two remedies, and they are alternatives rather
than layers. **Name the target** is disclosure: the shell states which Mesh
or Agent Node the destination is about, and the user reads the target before
acting. **Capture it** is a mode where the destination holds a subject the
selection no longer controls, and the user pins it there deliberately.

The pin shipped with the escape hatch ADR-0029 described for itself, and it
never became one. In practice the pins were unused, and because "Pins are
session UI state rather than saved Mesh data" they were never persisted — so
removing them is a deletion with no migration, no stored rows to rewrite and
nothing to deprecate.

Meanwhile the surrounding scope work made the competing-source problem
sharper, not softer. The canvas now derives its effective scope once, from
the View Mode and the sidebar Mesh selection, with no fallback guess; Mesh
selection is sticky rather than cleared by a re-click; and the same selection
is what moves a Mesh- or Agent-owned Probe destination. That work pushes the
whole product toward a single source of truth for "what am I looking at".
Capture is the one mechanism that manufactures a second answer to that
question.

## Decision

### 1. The ADR-0029 lens contract is unchanged

The three ownership lenses stand as written: Host owns machine-wide provider
and runtime state, Mesh owns one repository root, Agent owns one Agent Node.
So does the destination mapping, in full — the Host-lens Usage destination,
the Agent-lens Agent Changes destination with its change set "relative to the
node's Base Ref / merge-base", the Mesh-owned Project Files destination whose
"changed-files view is `HEAD`-relative", and the mixed-ownership rule that
Project Files "uses the focused node's working tree only when that node
belongs to the Mesh". The mixed decision stays visible in the detail line
(`Repository root` or `Working tree: <node>`), and Agent lenses still identify
their parent Mesh.

Two structural consequences of ADR-0029 are likewise unchanged. A destination
still declares its ownership before it can ship: `PROBE_TAB_DEFINITIONS` remains
a complete record over the Probe destinations, so adding one is an ownership
decision first. And the header still shows the lens and the subject (`Host`,
`Mesh: <name>`, or `Agent: <name>`) — the disclosure half of ADR-0029's
"names the target or … capture it" is the half that is kept in full.

What changes is only that the per-destination decision set no longer includes
a pinning decision. Ownership, baseline, selection-following and statefulness
are still required; capture is no longer an option among them.

### 2. There is no captured subject, and the mode vocabulary is two-valued

A Probe destination's context mode is exactly `fixed` (a Host destination that
reads the machine) or `following` (a Mesh/Agent destination that tracks the
sidebar selection). There is no third value. A destination therefore cannot
hold a subject that disagrees with the selection, and the header's
"Following selection" state is the only such state a Mesh or Agent
destination has.

Removing capture is what makes the disclosure promise keepable. A destination
whose subject cannot drift from the selection has exactly one honest answer to
render, so naming that subject cannot go stale; and the ambiguity ADR-0029
found — an action's target changing underneath the user — is answered by
reading the label rather than by trusting a mode the user has to remember.

When a destination's subject disappears, it renders the explicit
unavailable-context empty state. It does not resolve a neighbouring subject,
and it needs no unpin affordance to escape one.

### 3. Disclosure is the anti-drift guarantee, and the label carries it

The guarantee is confirmed by behaviour rather than assumed: with two Agent
Nodes in one Mesh, moving focus re-targets Agent Changes to the other node —
its subject label and its working directory both swap — and re-targets
Project Files to the other node's working tree while the Mesh stays put. The
Mesh did not change and the header did not move, but the subject text in it
did, so the swap is visible in place.

The subject label was promoted to a legible weight for exactly this reason. It
replaces capture as the thing a user reads before acting.

### 4. If disclosure proves too subtle, the next step is a drift signal — not capture

Should the promoted label turn out to be too easy to miss, the next step is an
explicit drift signal on the two diff-bearing destinations, Agent Changes and
Project Files: something that fires at the moment the subject swaps under a
stationary Mesh, so the change is announced where it happens. It is
explicitly **not** a return to capture.

The structural reason for that boundary is worth keeping. Promoting a label
changes text in place, while the failure mode is that the subject swaps with
the Mesh selection unchanged and nothing else in the header moves — so a
signal attached to how the label *looks* cannot cover the case the signal is
for. It has to be attached to the swap. The independent implementation of the
label promotion reached this same conclusion from that structure, which is why
the commitment is recorded here rather than left to the next person who finds
the label subtle.

## Alternatives considered

- **Keep Probe Context Pins.** Rejected: with the sidebar selection as the
  single source of truth for the canvas scope, a captured destination is a
  second scope by construction. Resolving the original ambiguity then requires
  using the ambiguous mechanism to escape it.
- **Capture implicitly** — snapshot the subject whenever a destination opens,
  or capture per subview without an affordance. Rejected for the same reason
  and one more: an invisible captured subject returns the ambiguity while
  removing the user's ability to see that it exists.
- **Carry the subject in the destination identity** (a deep link or route that
  encodes the subject). Not decided here. The `probe-<tab>` destination ids
  stay stable for callers and deep links, and encoding a subject in them is a
  separate contract question from whether the UI may hold a subject the
  selection does not own.
- **Drop the subject label and rely on the picker.** Rejected: naming the
  subject is the remedy ADR-0029 identified first, and it costs nothing at
  runtime. The selection is the thing a user changes silently, so the label is
  where the answer has to be.

## Consequences

- ADR-0029's "stable, user-visible escape from live selection through the pin
  control" is gone. Disclosure replaces it: the same information is always
  present, so it never has to be summoned.
- The pure-resolution test surface ADR-0029 promised still holds, with stale
  pins replaced by the subject-disappears cases: selection changes, missing
  subjects, and unavailable contexts are all covered without mounting a
  destination's data view.
- Follow-up destination work passes the resolved ids and the resolved subject
  label through its actions and async guards, exactly as ADR-0029 required —
  the ids now change only when the selection changes.
- The canvas side of the same single-source-of-truth decision: one derived
  scope value shared by the grid, the empty-state counts, keyboard traversal
  and the title-bar scope indicator; a Mesh Grid with no Mesh selected is an
  explicit empty state rather than a guessed Mesh.

## Verification evidence

- Agent Changes re-targets subject and working directory on every focus change,
  with two Agent Nodes in one Mesh: `follows the focused Agent Node: Agent
  Changes re-targets on every focus change` in
  `tests/unit/use-probe-context.test.tsx`.
- Project Files moves its working tree while the Mesh subject stays put:
  `follows the focused Agent Node: Project Files re-targets its working
  directory`, same file.
- A lost subject resolves to unavailable rather than to another subject:
  `reports a lost Agent Node as unavailable instead of resolving another
  subject`, same file.
- The mode vocabulary is two-valued at the type level (`ProbeContextMode` in
  `src/lib/probeContext.ts`).