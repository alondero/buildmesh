# PRD: Git Synchronization and Unified Changed Files Panel

## Problem Statement

Users of Buildmesh experience friction when managing Git repositories for their Meshes and Agent Nodes. 
1. **Stale Baselines:** There is no easy way to synchronize a Mesh's parent repository from the UI. Consequently, users frequently spawn Agent Nodes on top of a stale local branch, leading to outdated baselines (e.g., being 17 commits behind main).
2. **Hidden/Buried Changes:** Uncommitted changes in Agent Nodes are represented by a tiny pill in the node's title bar that is easily missed. Furthermore, there is no centralized panel showing a list of changed files with diff statistics (additions/deletions), forcing users to dig through a full hierarchical file explorer tree to find what has changed.
3. **Spawning Blockers:** Spawning a branched Agent Node is blocked if the parent Mesh has uncommitted changes, even though Git's worktree model isolates changes and allows this natively.

## Solution

We will introduce a frictionless Git synchronization and changed-files visualization workflow:
1. **Mesh-Level Git Sync:** A refresh/sync button next to the "Add Node" split button in the sidebar Mesh header, showing a visual badge of the number of commits behind (e.g., `↓17`). Clicking it pulls and fast-forwards the active branch.
2. **Auto-Sync on Node Spawn:** Automatically fetch and attempt to fast-forward the Mesh branch before spawning a new Agent Node if the repository is clean, falling back gracefully to local HEAD if the sync fails.
3. **Relax Spawning Constraints:** Remove the cleanliness check that blocks spawning branched Agent Nodes when the parent Mesh has uncommitted changes.
4. **Unified Changed Files Sidebar:** A collapsible "Changed Files" section at the top of the File Explorer Panel showing a flat list of modified files with additions/deletions stats (e.g., `+13 -4`). Clicking a changed file displays its diff inline.

## User Stories

1. As a developer, I want to see a sync button in the sidebar Mesh header, so that I can pull updates from the remote with a single click.
2. As a developer, I want to see a specific count of behind commits (e.g., `↓17`) next to the sync button, so that I instantly know when my local repository is out of date.
3. As a developer, I want the sync button to spin and show feedback when clicked, so that I know the operation is running and whether it succeeded or failed.
4. As a developer, I want to spawn a new Agent Node and have the repository automatically fast-forward to the latest remote commit if my local branch is clean, so that my agents never start work on stale baselines.
5. As a developer, I want the node spawn command to proceed offline/fallback to local HEAD if the remote sync fails, so that a network absence doesn't block me from creating nodes.
6. As a developer, I want to spawn branched Agent Nodes even if my parent Mesh has uncommitted changes, so that I don't have to stash or commit my active files just to spawn an agent.
7. As a developer, I want to open the File Explorer Panel for an Agent Node or Mesh and see a collapsible "Changed Files" section at the top, so that I can immediately see what has changed without hunting through the file tree.
8. As a developer, I want to see green/red diff statistics (e.g., `+13 -4`) for each changed file in the panel, so that I understand the scale and nature of the edits at a glance.
9. As a developer, I want to click any file in the "Changed Files" list to view its colored side-by-side diff view inline, so that I can quickly review my or the agent's work.
10. As a developer, I want the File Explorer Panel to display a full directory tree below the "Changed Files" section, so that I can still open or examine unmodified files in my editor.

## Implementation Decisions

### Modules to Build or Modify

- **Backend Commands (`src-tauri/src/commands/git.rs`):**
  - Extend `GitStatus` struct to include `additions` and `deletions` fields.
  - Implement a new `get_git_branch_status` command that checks the current branch, checks if an upstream branch is configured, and queries ahead/behind counts using `repo.graph_ahead_behind`.
  - Update `get_git_status` to compute additions and deletions for each file by performing a patch diff print (`diff_tree_to_workdir_with_index`).
- **Backend Spawn Logic (`src-tauri/src/agent/spawn.rs`):**
  - Read `base_ref` from `MeshConfig` and pass it to `create_git_worktree`.
  - Resolve the `base_ref` (with fallback logic) to a commit inside `add_worktree_impl` and use it to set the worktree HEAD.
  - Perform `git pull --ff-only` on the parent repository during spawn if the repository has no uncommitted changes. Fall back with a logged warning if pulling fails.
  - Remove the cleanliness check (`check_source_branch_clean`) during branched worktree spawning.
- **Frontend Sidebar Header (`src/components/Sidebar/MeshItem.tsx`):**
  - Call `get_git_branch_status` on mount, window focus, and on `GIT_CHANGED` events.
  - Render a sync/refresh icon to the left of the `NodeCreationForm` split button.
  - Show a text indicator (e.g., `↓17`) next to the sync button if the branch is behind.
- **Frontend Explorer Sidebar (`src/components/FileTree/FileExplorerPanel.tsx`):**
  - Fetch changed files for the selected path using `getGitStatus`.
  - Render a collapsible "Changed Files" (or "Modified Files") section at the top of the sidebar.
  - List changed files with their status badge and `+A -D` counts.
  - Bind clicking a changed file to show the diff view. Render the `FileTree` section below it.

### Data Model and API Changes

#### `GitStatus` (Rust/TypeScript)
```typescript
interface GitStatus {
  path: string;
  status: 'modified' | 'added' | 'deleted' | 'renamed' | 'untracked';
  additions: number;
  deletions: number;
}
```

#### `GitBranchStatus` (Rust/TypeScript)
```typescript
interface GitBranchStatus {
  name: string;
  ahead: number;
  behind: number;
}
```

## Testing Decisions

### Test Strategy
- **Rust Integration Tests:**
  - Test the `get_git_branch_status` command in a mock git repository with ahead/behind branches.
  - Test `get_git_status` in a mock repository to verify that additions/deletions match the line edits.
  - Test `create_git_worktree` starting from a custom base reference (like a branch or commit OID) rather than just `HEAD`.
- **Frontend Component Tests:**
  - Test that the collapsible "Changed Files" list correctly triggers `onFileSelect` with the computed file diff.
  - Test that the Sidebar `MeshItem` sync button spins during a sync call and displays the returned status text correctly.

## Out of Scope
- Full interactive git merge/rebase conflict resolution screens in the UI. If a fast-forward merge fails, the user is expected to handle it via a terminal.
- Support for syncing non-Git meshes.

## Further Notes
- We must ensure WSL path mappings are respected when invoking `get_git_status` and `git_sync` on WSL paths, utilizing the existing `to_host_path` helper.
