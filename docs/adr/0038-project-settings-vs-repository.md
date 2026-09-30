# 38. Project Settings and Repository are separate destinations

## Status

Accepted (2026-09-30). Implements #1460 on top of #1456 (lenses) and #1375
(title-bar navigation with an on-demand inspector).

## Context

Two Probe destinations mixed four different kinds of control:

- **Mesh Properties** owned identity, agent defaults, build and run commands,
  AI-context portability, and Delete Mesh.
- **Worktree Manager** owned a worktree *configuration* card (use-worktree,
  starting point, worktree mode, pre-spawn pool, worktree directory) **and**
  the Git *maintenance* surface (health, recovery, branch/worktree cleanup,
  remote-tracking prune).

Two problems followed from that. First, risk level was unreadable: a
checkbox that decides where new agent worktrees are cut sat directly above a
button that deletes branches. Second, the worktree strategy was reachable only
by entering a maintenance-heavy surface, so an ordinary configuration decision
carried the implicit risk of the destructive controls next to it.

The two destinations also disagreed about path semantics. Maintenance walks the
Mesh root; configuration edits the Mesh root; but a focused Agent Node's
worktree is a third path, and the body of each destination only implied which
one was in play.

## Decision

**Split by job, not by feature area.** Configuration and maintenance never
share a destination.

| Destination | Id | Owns |
| --- | --- | --- |
| Project Settings | `properties` | Identity and directory, agent runtime and harness defaults, build and run, worktree strategy (use-worktree, starting point, worktree mode, pre-spawn pool, worktree directory) |
| Repository | `worktrees` | Health and recovery, branch/worktree cleanup, remote-tracking prune |

`worktrees` is the destination that keeps the "Worktree Manager" role, and it
is renamed **Repository** because configuration no longer lives there — the old
name promised a surface the destination does not offer.

Three supporting decisions:

1. **Sections are the contract, not decoration.** Both destinations render
   labelled `ProbeSection` groups. Project Settings reads General → Agent
   runtime → Build and run → Worktree strategy → Danger zone; Repository
   separates Health and recovery from Branches and worktrees, because "restore
   the root" and "delete 4 branches" are not the same class of click. Delete
   Mesh sits in a `tone="danger"` zone that states its impact and its recovery
   story *before* the shared confirmation repeats the scope.

2. **Path semantics are stated, not inferred.** Project Settings says its
   Directory field is the project root; Repository says it acts on the project
   root and not the focused agent's worktree. The inspector header's
   `detailLabel` still covers the common case, but the body carries the
   statement so it survives a narrow dock.

3. **No permanent navigation is added.** Both remain on-demand inspector
   destinations. They are reachable from the command palette and from one
   title-bar overflow disclosure beside Usage, Settings, and Mobile — not from
   a rail, and not from two extra always-visible pills.

   Remote Access is listed alongside these as a global utility, but it already
   satisfies the rule: it is a labelled title-bar utility and is not a Probe
   destination. It therefore keeps its own **Mobile** pill rather than moving
   into the overflow — demoting a single-purpose, security-relevant control
   behind a disclosure would cost discoverability for no gain, and the issue
   is about splitting two destinations, not relocating Remote Access.

## Why the ids did not change

`properties` and `worktrees` are kept even though the labels moved. ADR-0030
keeps `probe-<tab>` command ids stable for existing callers and deep links, and
`useUIStore.openProbeTab` is called from the sidebar badges, the omnibar, and
the inspector's own chip list. Renaming the ids would churn every one of those
call sites to buy a naming symmetry that no user sees. The source files are
named after the destinations (`ProjectSettingsTab.tsx`,
`RepositoryTab.tsx`) so the code tree matches the information architecture
even though the wire ids do not.

## Consequences

- Every worktree-strategy control now has one home. The Repository destination
  imports no `updateMesh*` wrapper, and a regression test asserts the strategy
  controls are absent from it.
- Worktree-strategy saves keep their section-local error channel: in a form
  this long the message has to name the control that refused the write, and
  the "form mirrors user intent" rule means the field keeps the user's value
  while the message explains why it did not stick.
- The pre-split behaviour where a healthy project rendered *no* health block
  is gone. An absent section was indistinguishable from "not checked yet", so
  Repository now always renders the health section with an explicit clean
  state.
- Configuration storage, the auto-save model, and every IPC wrapper are
  unchanged. This is an information-architecture change, not a settings
  change.
