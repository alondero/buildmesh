# Buildmesh Context

Buildmesh is an orchestration platform for AI coding agents that work in parallel across meshes (repository roots) using Git worktrees.

## Language

**Mesh**:
A project workspace associated with a local Git repository root path.
_Avoid_: Project, repo, folder

**Agent Node**:
An interactive panel running a single agent execution process within a dedicated worktree or directory.
_Avoid_: Session, pane, terminal node

**File Explorer Panel**:
A collapsible side panel displaying files and changes for a given Mesh or Agent Node.
_Avoid_: File tree panel, sidebar drawer

**Base Ref**:
The Git reference a new Agent Node's worktree is created from (default `origin/main`). Configured per Mesh; surfaced in the UI as "Fresh" (the Base Ref) vs "Head" (the Mesh's current checkout).
_Avoid_: Base branch, starting point, source branch

**Changed Files Section**:
A distinct view in the File Explorer Panel listing modified files with their addition/deletion line counts.
_Avoid_: Modified files list

## Relationships

- A **Mesh** can have one or more **Agent Nodes**
- An **Agent Node** operates on a child worktree or branch of its parent **Mesh**
- A **File Explorer Panel** shows context for either a **Mesh** or an **Agent Node**

## Example dialogue

> **Dev:** "When the user spawns a new **Agent Node** under a **Mesh**, does it create a new Git branch?"
> **Domain expert:** "Yes, it creates a dedicated worktree branch tracking the selected starting point of that **Mesh**."

## Flagged ambiguities

- "session" and "node" were used interchangeably. Resolved: we canonicalize on **Agent Node** for the user interface and domain model, while database/backend can use "session" for process lifecycle records.
- "state pollution" in worktrees. Resolved: Git worktrees are fully isolated, so parent Mesh cleanliness is not required when spawning new Agent Nodes.
