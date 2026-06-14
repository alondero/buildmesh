# Buildmesh Context

Buildmesh is an orchestration platform for AI coding agents that work in parallel across meshes (repository roots) using Git worktrees.

## Language

**Mesh**:
A project workspace associated with a local Git repository root path.
_Avoid_: Project, repo, folder

**Agent Node**:
An interactive panel running a single agent execution process within a dedicated directory (either a worktree or the mesh root).
_Avoid_: Session, pane, terminal node

**Worktree Node**:
An Agent Node operating on an isolated Git worktree branch of its parent Mesh. (Used when the Mesh property use_worktree is true, unless overridden).

**Root Node**:
An Agent Node operating directly on the parent Mesh's root directory, bypassing worktree isolation. (Used when the Mesh property use_worktree is false, or when overridden via Alt-click).

**Node Working Directory**:
The directory an Agent Node's work physically lives in: its Worktree Node dir (`.claude/worktrees/<name>`) for a Worktree Node, or the Mesh root for a Root Node. The canonical "where is this node's stuff" rule (resolve `use_worktree` + a trimmed, non-empty `worktree_name`) lives in one place; callers pick the host form (Windows git2) or the spawn form (the path as the agent saw it — Linux for a WSL node, which is the form Claude Code encodes for its on-disk transcript directory).
_Avoid_: working path, repo path, node dir

**File Explorer Panel**:
A collapsible side panel displaying files and changes for a given Mesh or Agent Node.
_Avoid_: File tree panel, sidebar drawer

**Base Ref**:
The Git reference a new Agent Node's worktree is created from (default `origin/main`). Configured per Mesh; surfaced in the UI as "Fresh" (the Base Ref) vs "Head" (the Mesh's current checkout).
_Avoid_: Base branch, starting point, source branch

**Changed Files Section**:
A distinct view in the File Explorer Panel listing modified files with their addition/deletion line counts.
_Avoid_: Modified files list

**Drifted root**:
A Mesh whose root HEAD is not on the Base Ref's branch (e.g. the user parked the root on `feat/x` and forgot) — or is detached on a non-base commit. Surfaces as an amber `!` badge in the sidebar; one-click fix is "Restore root to base" in the mesh properties panel.
_Avoid_: Wrong branch, off branch, out of sync

**Base branch hostage**:
A condition where the Base Ref's branch (e.g. `main`) is checked out in one of the Mesh's worktrees, blocking `git checkout main` from the root. The health block names the holding worktree; the one-click fix is "Free base branch (worktree-name)".
_Avoid_: Branch locked, branch busy

**Unpushed commits on root**:
A Mesh whose root branch has local commits that aren't on its upstream — or has no upstream at all. The "Restore root to base" button refuses until the user pushes, branches, or resets the work, because a checkout would strand those commits in reflog.
_Avoid_: Local commits, un-pushed work

**Coordinator**:
An external, agent-agnostic supervisor that reads node state and drives nodes through Buildmesh's control API, rather than via the UI. The first coordinator is the user's remotely-hosted Hermes Agent (Nous Research); a future in-app "Buildmesh superagent" is intended to be a second coordinator on the same API. Buildmesh stays a "dumb" driver — the orchestration intelligence lives in the Coordinator.
_Avoid_: Supervisor, orchestrator agent, Hermes (Hermes is one instance of a Coordinator, not the category)

**Node Digest**:
A coordinator-facing read summary of a single Agent Node answering "what's going on, and does it need feedback?". Layered: an always-available spine from Buildmesh's own DB (lifecycle `status`, "needs feedback" = `awaiting_input`) enriched, for the Claude Code provider family only, with semantic content read from the agent's on-disk JSONL transcript. Non-supporting providers, or a transcript that fails to parse, degrade to the spine with the enrichment explicitly flagged unavailable (never silently omitted). The rendered terminal/TUI is deliberately **not** a digest source.
_Avoid_: Node summary, status payload, snapshot

## Relationships

- A **Mesh** can have one or more **Agent Nodes**
- An **Agent Node** operates on a child worktree or branch of its parent **Mesh**
- A **File Explorer Panel** shows context for either a **Mesh** or an **Agent Node**
- A **Mesh** can have a **drifted root** if its root HEAD is not on the Base Ref's branch
- A **Mesh** can be in a **base branch hostage** state when one of its worktrees holds the Base Ref's branch
- A **Mesh** can have **unpushed commits on root** that block the recovery actions

## Example dialogue

> **Dev:** "When the user spawns a new **Agent Node** under a **Mesh**, does it create a new Git branch?"
> **Domain expert:** "Yes, it creates a dedicated worktree branch tracking the selected starting point of that **Mesh**."

## Flagged ambiguities

- "session" and "node" were used interchangeably. Resolved: we canonicalize on **Agent Node** for the user interface and domain model, while database/backend can use "session" for process lifecycle records.
- "state pollution" in worktrees. Resolved: Git worktrees are fully isolated, so parent Mesh cleanliness is not required when spawning new Agent Nodes.
