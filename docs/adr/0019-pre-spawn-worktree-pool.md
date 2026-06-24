# 19. Pre-spawn Worktree Pool

To eliminate the 10.5-second cold-write latency caused by Windows Defender and NTFS USN journaling on fresh `git worktree add` operations, Buildmesh maintains a persistent pool of pre-warmed worktrees (pre-spawn worktrees) in a detached HEAD state. 

When a spawn is requested, Buildmesh grabs a pre-spawn worktree, renames it, and performs a fast local checkout or delta update in under 500ms.

## Context

On Windows machines (specifically measured on the `buildmesh-dev` profile in June 2026), spawning an agent node on a cold repository path adds a substantial "cold write" overhead (~10.5 seconds on a 400MB repository like `pixelpath`) compared to a raw shell `git worktree add` (~700ms). This overhead is due to the OS and antivirus (Windows Defender, search indexer, USN journal) scanning hundreds of newly created files on the cold NTFS filesystem.

Subsequent warm checkouts on the same path take ~500ms because the files and MFT entries are cached by the OS. 

To hide this filesystem cost from the user, we pre-warm a pool of worktrees in the background while the app is idle.

## Decision

We will:
1. **Define a persistent SQLite table `warm_worktrees`**:
   Tracks the pre-spawn worktrees currently sitting on disk. This enables crash safety and persistence across sessions:
   ```sql
   CREATE TABLE warm_worktrees (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       mesh_id INTEGER NOT NULL REFERENCES meshes(id),
       worktree_name TEXT NOT NULL UNIQUE,
       path TEXT NOT NULL UNIQUE,
       status TEXT NOT NULL, -- 'filling', 'available', 'refreshing'
       base_ref TEXT NOT NULL,
       resolved_sha TEXT NOT NULL,
       created_at TEXT NOT NULL DEFAULT (datetime('now')),
       last_refreshed_at TEXT NOT NULL DEFAULT (datetime('now'))
   );
   ```
2. **Opt-in configuration per Mesh**:
   Add a `pre_spawn_pool_size` column to the `meshes` table (default `0`, meaning disabled). If set to `1` or more, the pre-spawn pool is enabled for that Mesh. This keeps the disk footprint small for fast repos and only runs on slow repos where the user opts in.
3. **Detached HEAD State in the Pool**:
   To avoid polluting the user's local Git branch namespace, all pre-spawn worktrees are created in a detached HEAD state (`git worktree add --detach <path> <sha>`).
4. **Adoption and Checkout at Spawn**:
   When the user spawns a node:
   - If an `available` pool entry exists:
     - We run a fast transaction to claim the pool entry.
     - **For manual spawns**: We adopt the pool entry's pre-assigned random name (e.g. `gold-rough-rose`) as the node's name, eliminating any folder rename overhead.
     - **For Issue/PR spawns**: We rename the folder (e.g. to `gh123-fix-bug`) and run a fast `git worktree move` (~50ms).
     - We run `git checkout -b <branch>` (for branched mode) or `git checkout --detach` (for detached mode) to align with the user's worktree configuration.
     - We run `.worktreeinclude` copy tasks (e.g. `.env` file copying) in <2ms.
   - If the pool is empty or missing, we gracefully fall back to a standard cold spawn (no blocking).
5. **Background Refreshing and Throttling**:
   - The background worker refills the pool when the app is idle (debounced by 5 seconds of silence on PTY reader queues).
   - If a new commit is fetched in the parent Mesh, the pool manager runs a background delta update (`git reset --hard <new_sha>`) on the warm worktree. Since most files are unchanged, this update is extremely fast.
6. **Integration with Worktree Manager**:
   - The `is_pool` boolean field is added to `WorktreeInfo`.
   - `get_git_prune_info` includes pool entries and flags them as active/pool entries, preventing them from being deleted as stale worktrees.

## Consequences

- **Pros:** 
  - Cold spawn times drop from ~11 seconds to ~500ms.
  - 100% compliance with user's branched/detached worktree configuration at spawn time.
  - Zero namespace branch pollution while worktrees are sitting in the pool.
  - Opt-in configuration per Mesh prevents wasted disk space on fast repositories.
  - Full persistence across app launches: no startup overhead if the pool is already populated.
- **Cons:** 
  - Adds minor disk space overhead (typically ~400MB per pre-spawn worktree for medium repositories).
  - Minor complexity in the database schema and background tick loop.
- **Scope:**
  - Standard and Issue/PR spawns benefit. Autopilot spawns are also accelerated if a pool is pre-warmed.
