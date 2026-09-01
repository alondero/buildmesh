# 29. Probe context lenses and scope ownership

## Status

Accepted for the Probe shell contract (2026-09-01). Individual destination
migrations and the future grouped rail remain follow-up work.

## Context

The Probe activity rail contains views with three different kinds of subject:
machine-wide provider/runtime state, a Mesh and its repository, and one Agent
Node. The previous shell exposed the selected Mesh name for every destination,
so the Usage view could look Mesh-owned. It also left the Project Files path
and the Agent Changes baseline implicit: the former can follow a focused node's
working tree, while the latter is always the node's change set since its Base
Ref.

Selection is also live UI state. A Mesh or Agent Node can change while a
stateful view remains mounted, which makes an action's target ambiguous unless
the shell names the target or the user can capture it.

## Decision

Buildmesh uses three Probe ownership lenses:

| Lens | Subject | Owns |
| --- | --- | --- |
| Host | The local machine and installed/authenticated integrations | Provider usage, account/runtime health, and other machine-wide state |
| Mesh | One Mesh repository root | Configuration, repository state, worktrees, GitHub feeds, notes, and automation |
| Agent | One Agent Node | Node changes, logs/history, resume actions, and node-specific artifacts |

The authoritative frontend contract is
`src/lib/probeContext.ts`. `PROBE_TAB_DEFINITIONS` is a complete
`Record<ProbeTab, ProbeTabDefinition>`, so adding a destination requires an
ownership, baseline, selection, pinning, and statefulness decision. The
`useProbeContext` hook is the read seam used by the shell and destination
implementations.

### Destination mapping

| Probe destination | Ownership lens | Baseline / mixed-ownership decision |
| --- | --- | --- |
| Project Files (`files`) | Mesh | Mesh-owned repository view; it uses the focused node's working tree only when that node belongs to the Mesh. Its changed-files view is `HEAD`-relative. |
| Agent Changes (`review`) | Agent | One focused Agent Node; its change set and diffs are relative to the node's Base Ref / merge-base. |
| Usage (`usage`) | Host | Host-global. It never resolves or displays a Mesh or Agent subject. |
| Worktree Manager (`worktrees`) | Mesh | Mesh worktree inventory and maintenance actions. |
| Mesh Properties (`properties`) | Mesh | Mesh configuration, including destructive Mesh actions. |
| Autopilot (`autopilot`) | Mesh | Mesh Autopilot policy and status. |
| Circuits (`circuits`) | Mesh | Mesh-owned Circuit blueprints and runs. |
| Git Issues (`issues`) | Mesh | Mesh GitHub feed and issue-to-node actions. |
| Pull Requests (`pulls`) | Mesh | Mesh GitHub feed and PR actions. |
| Archive (`sessions`) | Mesh | A Mesh-owned index of Agent Node history. Each row is an Agent Node and resume acts on that row; the destination itself is not keyed to the currently focused node. |
| Scratch Pad (`scratchpad`) | Mesh | Notes are persisted on the Mesh, not on the focused Agent Node. |

### Following selection and pinning

Host destinations are fixed to Host and ignore Mesh/Agent selection. The
current Mesh and Agent destinations follow the relevant selection by default.
The shell header must show the lens and subject (`Host`, `Mesh: <name>`, or
`Agent: <name>`) and must show `Following selection` while it does so.

Every non-Host destination is pinnable when it has a target. Pinning stores
only stable IDs, is local to the destination, and changes the header to
`Pinned context`. A pinned destination never falls back to a newly selected
Mesh or Agent Node. If its subject disappears, the destination renders an
explicit unavailable-context empty state and offers unpinning; it must not
silently operate on a different subject. Pins are session UI state rather than
saved Mesh data.

The mixed Project Files decision is visible in the detail line (`Repository
root` or `Working tree: <node>`). Agent lenses also identify their parent Mesh
when it is available. This preserves the existing tab behavior while making
the secondary dependency explicit ahead of the follow-up tab migrations.

## Consequences

- Usage cannot inherit a Mesh name from the shared dock header.
- A tab's context behavior is testable without mounting its full data view:
  pure resolution covers selection changes, missing subjects, and stale pins.
- Stateful Mesh destinations have a stable, user-visible escape from live
  selection through the pin control. Follow-up changes must pass the resolved
  IDs from this contract into their actions and async guards.
- The future categorized rail (#1375) can group destinations by `lens`
  without re-deriving ownership. It does not belong in this contract change.
