# 6. Mesh Health Detection and One-Click Recovery

Status: accepted

A Mesh's Git state can drift into a blocking condition without the user noticing: the root can be left on a feature branch, on a detached HEAD, or holding a branch that another worktree is also trying to check out. Buildmesh now detects these states in a single `MeshHealth` snapshot and surfaces them as a persistent amber `!` badge in the sidebar, with manual, guarded one-click fixes in the mesh properties panel. **No automatic HEAD movement at any point** — recovery is always user-initiated.

## Context

The "behind upstream" `↓N` badge in `MeshItem.tsx` is computed by `get_git_branch_status` only against the **current branch's upstream**. If the root is on a feature branch with no upstream (the typical drift state), `behind = 0` and the badge is **invisible** — even though the mesh is in the most drift-y state possible. A separate "off base branch" condition needs its own surface.

Three failure modes are user-visible and recoverable:

1. **Drifted root** — HEAD not on the Base Ref's branch (e.g. the user parked the root on `feat/x` and forgot); includes detached HEAD on a non-base commit.
2. **Base branch hostage** — the Base Ref's branch is checked out in one of the worktrees, blocking `git checkout <base>` from the root. This is git's invariant: only one worktree may have a given branch checked out at a time.
3. **Unpushed / unsaved work on root** — dirty working tree, ahead-of-upstream commits, or local commits on a branch with no upstream. Any "restore to base" action would strand this work, and recovery must refuse rather than silently lose it.

Sister issue #230 made `base_ref` load-bearing for new worktree creation. This ADR makes it load-bearing for **recovery** of meshes that have already drifted, and adds the UX so the user can see the problem and fix it without leaving the app.

## Decision

1. **One snapshot, one source of truth.** A single `MeshHealth` struct (`base_ref`, `local_base_branch`, `current_branch`, `is_detached`, `is_dirty`, `unpushed_ahead`, `has_upstream`, `is_drifted`, `base_branch_holder`) is computed by `compute_mesh_health(&Repository, &str)` — a pure helper with no DB access, so it can be unit-tested against real temp repos. The Tauri command `get_mesh_health(mesh_id)` adds the live `active_paths` (from `db::list_agent_nodes`) to refine `base_branch_holder.is_active`. Every UI surface (sidebar badge, panel block, fix buttons) reads from the same struct so they cannot disagree.

2. **Local branch derivation.** `parse_local_branch(&str) -> Option<String>` accepts `origin/main` → `main`, `main` → `main`, `refs/heads/main` → `main`, and `origin/feature/foo` → `foo`. It rejects `HEAD` and `FETCH_HEAD` so the badge and the recovery button are suppressed when no real base branch is configured. The same helper backs issue #230's `base_ref` resolution.

3. **Drift detection rules, in priority order:**
   - `local_base_branch.is_none()` → `is_drifted = false` (no base configured)
   - `current_branch == Some(local_base_branch)` → `false` (on base)
   - Detached HEAD at the base branch's OID → `false` ("close enough" — no badge)
   - Otherwise → `true`

4. **`unpushed_ahead` semantic.** Counts local commits that would be stranded by `git checkout <base>`. When the branch has an upstream configured, use `graph_ahead_behind(tip, upstream)`; when no upstream, use `graph_ahead_behind(tip, local_base_tip)`. A branch with the same tip as the local base (a fresh branch with no local commits) reports `0` — it has nothing to lose.

5. **Recovery commands refuse, never silently fail.** `restore_mesh_to_base` and `free_base_branch` are the **only** state mutations. Both run a guard chain that short-circuits with a user-readable `Err` on the first failure:
   - `restore_mesh_to_base` rejects dirty roots, unpushed / no-upstream roots, already-on-base (no-op), and base-branch-hostage. The error message names the guard so the user knows which one fired. The "already on base" check must come **before** the hostage check: the root worktree itself "holds" the base branch whenever it's on it, and that's not a hostage — it's the desired state.
   - `free_base_branch` is idempotent (re-running on an already-detached worktree is a no-op success), non-destructive (`git checkout --detach` preserves the worktree's working tree, index, and HEAD commit), and refuses if the supplied path is not the current holder.

6. **Recovery surfaces in the mesh properties panel, not as a global action.** Both buttons live in the health block at the top of `BranchesWorktreesSection`. Their disabled state mirrors the backend guard chain, and the `title` attribute quotes the exact backend error message so the user knows why a button would refuse before they click it. The sidebar badge opens the panel — the badge click does **not** trigger recovery, so a misclick can't strand work.

7. **Recovery emits `git-changed` events** for the affected paths (the mesh path, plus the freed worktree's `internal_path`). The `useMeshHealth` hook and `useGitBranchStatus` both listen for this event, so the sidebar badge clears and the panel refreshes automatically after a successful recovery.

## Considered alternatives

- **Auto-restore on detection.** Rejected: a buildmesh-initiated `git checkout` is exactly the silent-data-loss the refuse-rule is designed to prevent. The user must always be in control of when, and whether, the root moves.
- **Force-restore with confirmation dialog.** Rejected: makes the destructive path a "yes I really mean it" flow. A user who clicks "Restore" while there are unpushed commits is *not* in a position to confidently answer that — they may not remember what's on the branch. Refusing-by-default puts the burden of *unlocking* the recovery on the user (push, branch, or reset first), which is a stronger guarantee than a confirmation dialog.
- **Detect drift ad-hoc in three places (sidebar, panel, recovery).** Rejected: the three places would have to agree about the mesh's state. A single `MeshHealth` snapshot computed server-side and read by all three eliminates the disagreement class of bug.
- **Put the fix buttons in the sidebar popover, not the panel.** Rejected: the sidebar has no precedent for invoking Tauri commands from a popover (the existing pattern is "click → open panel/modal"). A panel-side health block matches the rest of the file and avoids introducing a new UI primitive.

## Consequences

- **The sidebar `!` badge is now the primary "is this mesh OK?" indicator.** It appears even when the existing `↓N` lag badge is hidden (the no-upstream case), so a drifted mesh on a feature branch is no longer invisible. The two badges are complementary: `!` is structural, `↓N` is lag.
- **Detection lives in `MeshHealth`, not scattered across `get_git_branch_status` and `get_git_prune_info`.** A future contributor adding a new drift condition adds a field to the struct and a button to the panel — they don't have to invent a new surface or remember to update an existing one.
- **The refuse-rule is load-bearing.** Any refactor that "simplifies" `restore_to_base_impl` by removing a guard or by adding a force flag must update this ADR first. The comment in the code at the guard site is a backstop, but a written record is the only thing that survives a multi-year code churn.
- **WSL is handled by the existing `to_host_path` plumbing.** `get_mesh_health` and `free_base_branch` both call `to_host_path` on the incoming path before opening the repo, matching the pattern at `git.rs:99` and `prune.rs:28`. A WSL-stored mesh path resolves correctly on both sides of the host/guest boundary.
- **Worktree location caveat.** Agent worktrees live at `<mesh>/.claude/worktrees/<name>/`, which is inside the mesh root. The root's `is_dirty` check would otherwise see the worktree directory as untracked. In production this is mitigated by the buildmesh-side `.gitignore`; in tests, worktrees are placed in a sibling directory so the root stays clean for the dirty/unpushed guards.
