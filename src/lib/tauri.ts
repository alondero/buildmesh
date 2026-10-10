import { Channel } from '@tauri-apps/api/core';
import { _invoke } from './tauri/_invoke';
import { emit } from '@tauri-apps/api/event';
import { deleteDefaultProviderPromise, clearDefaultProviderPromises } from './providerCache';
// Issue #1530 — the one ordered retry buffer for PTY input, owned here at
// the transport seam. It is the only thing that reacts to a `backpressured`
// disposition, so no call site can invent its own recovery.
import { TerminalInputQueue, type InputStall } from './terminalInputQueue';
import type { InputOutcome } from '../types/generated/InputOutcome';
// The cross-surface invalidation event is owned by the provider facet
// (same layer); this facade re-emits it after default-provider writes
// so open spawn clusters refresh.
import { PROVIDER_LIST_CHANGED_EVENT } from './tauri/provider';
// Re-export every typed wrapper from the provider/harness facet (issue
// #1656 Phase 2 — first facet). The new `getResolvedHarnessView` IPC
// command lives here along with every other harness/provider wrapper.
// Re-exporting keeps the existing 62 importers compiling unchanged while
// new code can reach the resolver directly via `../lib/tauri/provider`.
export * from './tauri/provider';
import type { AgentNode } from '../stores/agentNodeStore';
import type { Mesh } from '../stores/meshStore';
import type { AiContextStatus } from '../types/generated/AiContextStatus';
import type { AppPreferences } from '../types/generated/AppPreferences';
import type { CorruptionInfo } from '../types/generated/CorruptionInfo';
import type { CorruptionReason } from '../types/generated/CorruptionReason';
import type { PreferencesHealth } from '../types/generated/PreferencesHealth';
import type { PreferencesStatus } from '../types/generated/PreferencesStatus';
import type { RecoveryOutcome } from '../types/generated/RecoveryOutcome';
import type { BranchInfo } from '../types/generated/BranchInfo';
import type { CoordinatorStatus } from '../types/generated/CoordinatorStatus';
import type { DeviceSession } from '../types/generated/DeviceSession';
import type { DiagnosticPaths } from '../types/generated/DiagnosticPaths';
import type { DiffHunk } from '../types/generated/DiffHunk';
import type { DiffLine } from '../types/generated/DiffLine';
import type { DiffResult } from '../types/generated/DiffResult';
import type { ArchivedAgentNode } from '../types/generated/ArchivedAgentNode';
import type { FileDiff } from '../types/generated/FileDiff';
import type { FileNode } from '../types/generated/FileNode';
import type { FreeResult } from '../types/generated/FreeResult';
import type { GitBranchStatus } from '../types/generated/GitBranchStatus';
import type { GitHubIssue } from '../types/generated/GitHubIssue';
import type { GitHubIssueFeed } from '../types/generated/GitHubIssueFeed';
import type { GitHubPullRequestFeed } from '../types/generated/GitHubPullRequestFeed';
import type { GitHubPullRequest } from '../types/generated/GitHubPullRequest';
import type { GitRepoPruneInfo } from '../types/generated/GitRepoPruneInfo';
import type { GitSummary } from '../types/generated/GitSummary';
import type { GitSyncResult } from '../types/generated/GitSyncResult';
import type { HoldingWorktree } from '../types/generated/HoldingWorktree';
import type { IssueNodeDraft } from '../types/generated/IssueNodeDraft';
import type { MeshRow } from '../types/generated/MeshRow';
import type { MeshGitStatic } from '../types/generated/MeshGitStatic';
import type { MeshHealth } from '../types/generated/MeshHealth';
import type { NetworkStatus } from '../types/generated/NetworkStatus';
import type { PendingWorktreeRemoval } from '../types/generated/PendingWorktreeRemoval';
import type { BlockingProcess } from '../types/generated/BlockingProcess';
import type { WorktreeCleanupRetryResult } from '../types/generated/WorktreeCleanupRetryResult';
import type { WorktreeCleanupDismissalResult } from '../types/generated/WorktreeCleanupDismissalResult';
import type { PickedFolder } from '../types/generated/PickedFolder';
import type { OpenPr } from '../types/generated/OpenPr';
import type { PrMergeability } from '../types/generated/PrMergeability';
import type { ProbeSpawnPromptDefaults } from '../types/generated/ProbeSpawnPromptDefaults';
import type { PrMergeabilityEntry } from '../types/generated/PrMergeabilityEntry';
import type { PrFileEntry } from '../types/generated/PrFileEntry';
import type { PrFileFeed } from '../types/generated/PrFileFeed';
import type { RealizedBind } from '../types/generated/RealizedBind';
import type { RestoreResult } from '../types/generated/RestoreResult';
import type { SpawnAgentRequest } from '../types/generated/SpawnAgentRequest';
import type { WorktreeInfo } from '../types/generated/WorktreeInfo';
import type { WorktreeCloseSafety } from './worktreeClose';

// `providerCache` import + `__resetProviderCachesForTests` test helper
// moved to `./tauri/provider` (issue #1656 Phase 2 — first facet). The
// facade re-exports it via the `export * from './tauri/provider'` line
// above so existing tests continue to work.
//
// `_invoke` and the raw `invoke` import moved to `./tauri/_invoke` (issue
// #1656 review) — the chokepoint is now the single allowed raw-invoke
// site under `src/lib/tauri/`, and every wrapper in `tauri.ts` (this
// file) routes through it just like the facets. The IPC seam test
// (`tests/unit/tauri-ipc-seam.test.ts`) enforces the property.

export type DiffLineType = 'context' | 'add' | 'remove';

// Diff types — generated from the Rust structs in `src-tauri/src/models/mod.rs`
// (issue #404). The Rust `status` / `line_type` fields are plain `String`, so
// the generated types emit `string` (a wider union than the hand-written
// versions used to carry); consumers that switch on them still compare fine.
export type { DiffLine, DiffHunk, FileDiff, DiffResult };

/** Change kind vocabulary, shared with `GitStatus.status`. Subset of the
 *  `FileDiff.status` string union the generated type uses. Kept as a hand-typed
 *  alias because it documents the closed set consumers can rely on. */
export type FileDiffStatus =
  | 'added'
  | 'modified'
  | 'deleted'
  | 'renamed'
  | 'untracked';

// Agent Node — renamed from `*Session` to `*AgentNode` in issue #490.
export const createAgentNode = (meshId: number, name: string, path: string, branch: string, provider?: string, useWorktree?: boolean, configurationId?: string) =>
  _invoke<AgentNode>('create_agent_node', { meshId, name, path, branch, provider, useWorktree, configurationId });

export const listAgentNodes = () =>
  _invoke<AgentNode[]>('list_agent_nodes');

export const getAgentNode = (nodeId: number) =>
  _invoke<AgentNode>('get_agent_node', { nodeId });

export const getWorktreeCloseSafety = (nodeId: number) =>
  _invoke<WorktreeCloseSafety>('get_worktree_close_safety', { nodeId });

export const deleteAgentNode = (nodeId: number, removeWorktree = false) =>
  _invoke('delete_agent_node', { nodeId, removeWorktree });

export const renameAgentNode = (nodeId: number, name: string) =>
  _invoke('rename_agent_node', { nodeId, name });

/** Persist new grid positions for a set of nodes: `[nodeId, position]` pairs. */
export const updateAgentNodePositions = (updates: [number, number][]) =>
  _invoke('update_agent_node_positions', { updates });

/** Pin / unpin an agent node for the Pinned Grid view (wayfinder #982 /
 * ticket #984). Used by the UI affordance when the user wants a
 * known-good state (e.g. "Pin this node" in a context menu); `toggle`
 * below flips whatever the current value is. Returns the post-write
 * `AgentNode` so the store can patch the local entry directly. */
export const setNodePinned = (nodeId: number, pinned: boolean) =>
  _invoke<AgentNode>('set_node_pinned', { nodeId, pinned });

/** Flip a node's `is_pinned` flag and return the post-write `AgentNode`
 * (wayfinder #982 / ticket #984). The single-action shape the UI's
 * click-to-pin button uses — the user doesn't need to know the current
 * pinned value, just "toggle". */
export const toggleNodePinned = (nodeId: number) =>
  _invoke<AgentNode>('toggle_node_pinned', { nodeId });

// Mesh — renamed from `*Project` to `*Mesh` in issue #490.
export const addMesh = () =>
  _invoke<Mesh>('add_mesh');

/** Open the native folder picker; returns the chosen folder or null (cancel). */
export const pickMeshFolder = () =>
  _invoke<PickedFolder | null>('pick_mesh_folder');

export const createMesh = (name: string, path: string, color?: string | null) =>
  _invoke<Mesh>('create_mesh', { name, path, color: color ?? null });

/** Clone a GitHub repo (`owner/repo` or a github.com URL) into
 * `<parentDir>/<repo>` and create a mesh from it in one step. */
export const cloneMeshRepo = (url: string, parentDir: string, color?: string | null) =>
  _invoke<Mesh>('clone_mesh_repo', { url, parentDir, color: color ?? null });

/** Set (or clear, with null) a mesh's accent colour hex. */
export const updateMeshColor = (meshId: number, color: string | null) =>
  _invoke('update_mesh_color', { meshId, color });

export const createTestMesh = (name: string) =>
  _invoke<Mesh>('create_test_mesh', { name });

export const listMeshes = () =>
  _invoke<Mesh[]>('list_meshes');

export const deleteMesh = (meshId: number) =>
  _invoke('delete_mesh', { meshId }).then((result) => {
    // Issue #2017 — the per-mesh promise maps below are keyed by Mesh id
    // and have no size bound, so a deleted Mesh kept its slot (and the
    // promise chain behind it) for the rest of the process. Evict on the
    // success path only: a rejected delete means the Mesh survives.
    // Unconditional rather than identity-checked, because a Mesh id can
    // be reused by SQLite after the highest row is deleted — dropping a
    // stale slot is exactly right for the Mesh that inherits the id.
    deleteDefaultProviderPromise(meshId);
    scratchpadByMesh.delete(meshId);
    scratchpadWritesByMesh.delete(meshId);
    return result;
  });

export const updateMeshLayout = (meshId: number, layout: 'grid' | 'single') =>
  _invoke('update_mesh_layout', { meshId, layout });

/** Persist new sidebar positions for a set of meshes: `[meshId, position]` pairs. */
export const updateMeshPositions = (updates: [number, number][]) =>
  _invoke('update_mesh_positions', { updates });

export const updateMeshName = (meshId: number, name: string) =>
  _invoke('update_mesh_name', { meshId, name });

// (Provider / harness wrappers moved to `./tauri/provider` (issue #1656
// Phase 2 — first facet). The `export * from './tauri/provider'` at the
// top of this file keeps every existing consumer compiling unchanged.)

// Mesh properties / configuration (issue #283)
//
// `MeshRow` is the wire shape of `commands::mesh_properties::get_mesh_properties`.
// It is a 1:1 mirror of the user-tunable columns on the `meshes` SQLite row
// (NOT a `mesh.toml` file — see `src-tauri/src/models/mod.rs::MeshRow` for the
// truth). Generated from `src-tauri/src/models/mod.rs` (issue #404 / issue #474).
export type { MeshRow };

export const getMeshProperties = (meshId: number) =>
  _invoke<MeshRow>('get_mesh_properties', { meshId });

/** Generic Mesh column write: writes `value` to the `meshes.<column>` row
 *  for `meshId` via the backend's `update_mesh_column`. **There is no
 *  `mesh.toml` file** — every field lives on the `meshes` SQLite row. The
 *  column parameter is validated against an allowlist on the backend; use
 *  it for fields with no settings.json side-effects (build_command,
 *  run_command, model, effort, worktree_mode, default_provider). Fields
 *  with side-effects have dedicated commands — `updateMeshUseWorktree` and
 *  `updateWorktreeBaseRef` below. */
export const updateMeshColumn = (
  meshId: number,
  column:
    | 'build_command'
    | 'run_command'
    | 'root_build_command'
    | 'root_run_command'
    | 'model'
    | 'effort'
    | 'worktree_mode'
    | 'default_provider',
  value: string,
) => _invoke<void>('update_mesh_column', { meshId, column, value }).then((result) => {
  // Write-through invalidation: the spawn clusters' quick-spawn icons
  // resolve via the cached getDefaultProvider. A successful per-mesh
  // default write must evict that mesh's entry (so the next read re-hits
  // IPC) and notify open surfaces via the shared invalidation event.
  if (column === 'default_provider') {
    deleteDefaultProviderPromise(meshId);
    void emit(PROVIDER_LIST_CHANGED_EVENT).catch(() => {});
  }
  return result;
});

export const updateMeshUseWorktree = (meshId: number, useWorktree: boolean) =>
  _invoke<void>('update_mesh_use_worktree', { meshId, useWorktree });

/** Toggle whether this mesh's agent nodes run inside an OS process sandbox
 *  (Windows restricted token #528 / macOS Seatbelt #497). Dedicated command (typed
 *  bool + zero-rows-is-an-error contract), like `updateMeshUseWorktree`.
 *
 *  Experimental and developer-gated (issue #2034): the backend ignores this
 *  flag entirely unless `BUILDMESH_SANDBOX=1`, so a value persisted by a dev
 *  build cannot confine processes in a release. */
export const updateMeshSandbox = (meshId: number, sandbox: boolean) =>
  _invoke<void>('update_mesh_sandbox', { meshId, sandbox });

/** Is the experimental agent sandbox available in this process (issue #2034)?
 *  Only gates whether the Mesh Sandbox toggle is *offered* â€” the spawn path
 *  re-checks `sandbox::sandbox_enabled`, so hiding the control here is not
 *  what keeps a release unsandboxed. */
export const sandboxDevModeEnabled = () =>
  _invoke<boolean>('sandbox_dev_mode_enabled');

/** Set the per-mesh limit on admitted Circuit Runs (1..8). */
export const updateMeshCircuitRunCapacity = (meshId: number, capacity: number) =>
  _invoke<void>('update_mesh_circuit_run_capacity', { meshId, capacity });

/** Set the mesh's pre-spawn worktree pool target. */
export const updateMeshPoolSize = (meshId: number, poolSize: number) =>
  _invoke<void>('update_mesh_pool_size', { meshId, poolSize });

/**
 * Returns the number of `available` warm pool entries for the given mesh —
 * the value behind the Worktrees Probe's per-mesh pool badge. Powers the
 * badge alongside `usePoolChanged`, which fires this on every
 * `pool-count-changed` event from the Rust pool service.
 *
 * Thin wrapper over `commands::mesh_properties::get_mesh_pool_count`,
 * which is itself a thin wrapper over `db::count_available_warm_for_mesh`
 * — the badge's source of truth is the DB row count, never a derived
 * value cached in TS state.
 */
export const getWarmPoolCount = (meshId: number) =>
  _invoke<number>('get_mesh_pool_count', { meshId });

export const updateWorktreeBaseRef = (meshId: number, baseRef: string) =>
  _invoke<void>('update_worktree_base_ref', { meshId, baseRef });

/** Set the per-Mesh Worktree Node directory override (issue #1519).
 *  Pass `null` (or blank, which the backend collapses) to clear and inherit
 *  the app default. Relative resolves from the Mesh root; absolute must be
 *  in the same environment (native vs WSL) or the backend rejects with an
 *  actionable message. Triggers a background warm-pool rebuild; live nodes
 *  keep their persisted `worktree_path`. */
export const updateMeshWorktreeDirectory = (meshId: number, directory: string | null) =>
  _invoke<void>('update_mesh_worktree_directory', { meshId, directory });

export type { WorktreeDirectoryConfig } from '../types/generated/WorktreeDirectoryConfig';

/** Effective worktree directory config for one Mesh (issue #1519) —
 *  override + app default + resolved effective container for the
 *  Project Settings → Worktrees inherited-value display. */
export const getWorktreeDirectoryConfig = (meshId: number) =>
  _invoke<import('../types/generated/WorktreeDirectoryConfig').WorktreeDirectoryConfig>(
    'get_worktree_directory_config',
    { meshId },
  );

// Scratch Pad (Probe Panel "📝 Scratch Pad" tab).
//
// Plain-text free-form notes per mesh. The empty string is a normal
// "no notes yet" state, not an error — `getMeshScratchpad` resolves
// to `""` for a fresh mesh so the editor mounts blank, and `setMeshScratchpad`
// accepts `""` as a clear-notes write. Debounced on the call site (~500ms)
// to keep the IPC chatter bounded while the user is mid-thought.
//
// The per-mesh promise cache mirrors the `getDefaultProvider` pattern
// (issue #405): concurrent callers de-dupe onto the in-flight promise,
// a rejection evicts the slot so the next caller retries, and `set`
// updates the cache so the editor sees its own writes without a
// round-trip. Notes are not cross-mesh shared, so the cache key is
// `meshId` (no need for a global slot).
const scratchpadByMesh = new Map<number, Promise<string>>();
const scratchpadWritesByMesh = new Map<number, Promise<void>>();

export const getMeshScratchpad = (meshId: number): Promise<string> => {
  let p = scratchpadByMesh.get(meshId);
  if (!p) {
    p = _invoke<string>('get_mesh_scratchpad', { meshId });
    const read = p;
    p.catch(() => {
      if (scratchpadByMesh.get(meshId) === read) scratchpadByMesh.delete(meshId);
    });
    scratchpadByMesh.set(meshId, p);
  }
  return p;
};

export const setMeshScratchpad = (meshId: number, content: string): Promise<void> => {
  // Keep writes ordered across mesh switches and component remounts. Reads
  // share the latest acknowledged write, never an unpersisted optimistic value.
  const write = () => _invoke<void>('set_mesh_scratchpad', { meshId, content }).then(() => undefined);
  const previous = scratchpadWritesByMesh.get(meshId);
  const p = previous ? previous.catch(() => {}).then(write) : write();
  scratchpadWritesByMesh.set(meshId, p);
  const read = p.then(() => content);
  scratchpadByMesh.set(meshId, read);
  read.catch(() => {
    if (scratchpadByMesh.get(meshId) === read) scratchpadByMesh.delete(meshId);
  });
  const clearWrite = () => {
    if (scratchpadWritesByMesh.get(meshId) === p) scratchpadWritesByMesh.delete(meshId);
  };
  p.then(clearWrite, clearWrite);
  return p;
};

import type { DetectedProject } from './projectPresets';

export const detectMeshProject = (meshPath: string) =>
  _invoke<DetectedProject>('detect_mesh_project', { meshPath });

// Agent
export const spawnAgent = (request: SpawnAgentRequest) =>
  _invoke('spawn_agent', { request });

// Issue #774 / #775 — swap a node's Model Provider. The worktree,
// branch, name, and position are preserved; only `provider` changes. The
// backend decides resume vs fresh from the new provider's harness.
export const regenerateAgentNode = (nodeId: number, newProviderId: string) =>
  _invoke<AgentNode>('regenerate_agent_node', { nodeId, newProviderId });

export const killAgent = (sessionId: number) =>
  _invoke('kill_agent', { sessionId });

export const isAgentRunning = (sessionId: number) =>
  _invoke<boolean>('is_agent_running', { sessionId });

export const sendToAgent = (sessionId: number, input: string) =>
  _invoke('send_to_agent', { sessionId, input });

/** Raw write to the agent's PTY (no submit/newline handling — cf. `sendToAgent`).
 *
 * Bypasses the ordered retry buffer on purpose: this is the transport
 * itself, used by the queue and by tests that need to observe a refused
 * write. Product callers use `writeToAgent` below.
 */
export const writeToAgentRaw = (sessionId: number, data: string) =>
  _invoke<InputOutcome>('write_to_agent', { sessionId, data });

const terminalInputQueue = new TerminalInputQueue({ write: writeToAgentRaw });

/**
 * Watch for a session whose input is being held rather than delivered.
 *
 * The transport owns the buffer, so the UI subscribes here instead of the
 * transport reaching into a store. Reports only once bytes have been held
 * past the queue's threshold, so a normal burst of typing shows nothing. The
 * `nodeId` scopes every report and every withdrawal to one session, so a
 * recovering agent cannot clear a badge another agent still needs.
 */
export const subscribeTerminalInputStall = (
  listener: (nodeId: number, stall: InputStall | null) => void,
) => terminalInputQueue.subscribeStall(listener);

/** Forget everything pending for a session (node closed, mesh torn down). */
export const cancelTerminalInput = (sessionId: number) => terminalInputQueue.cancel(sessionId);

/**
 * Write to the agent's PTY through the one ordered retry buffer (issue #1530).
 *
 * Every product caller uses this rather than the raw invoke, so no call site
 * can invent its own backpressure behaviour: the buffer preserves input
 * order per session, retries a refused write with the identical bytes (never
 * a duplicate — a refusal means the bytes were never queued), and settles
 * with the disposition that ultimately applied. A rejected IPC call still
 * rejects, so `pasteClipboard` can tell "could not try" from "tried and was
 * refused".
 */
export const writeToAgent = (sessionId: number, data: string) =>
  terminalInputQueue.enqueue(sessionId, data);

/**
 * Hand `text` over to an existing agent (`handover_to_agent`). Not a
 * `writeToAgent` convenience wrapper: the backend owns staging the text as one
 * bracketed paste and submitting it with a decoupled Enter, so a multi-line
 * handover cannot submit at every newline (issue #874).
 */
export const handoverToAgent = (targetNodeId: number, text: string) =>
  _invoke('handover_to_agent', { targetNodeId, text });

// Diff
export const diffFiles = (oldPath: string, newPath: string) =>
  _invoke<DiffResult>('diff_files', { oldPath, newPath });

export const diffFileAgainstHead = (sessionPath: string, filePath: string) =>
  _invoke<DiffResult>('diff_file_against_head', { sessionPath, filePath });

// Every file an agent changed since branching (merge-base with mesh base_ref;
// see ADR 0005). One call returns the whole change set for the review panel.
//
// Issue #1181 — cancellation seam: `signal` is accepted so a component
// that issues overlapping `fetchDiff` calls can pass a per-call
// `AbortSignal` and have the local `.then` short-circuit if a newer
// request has superseded it. Tauri 2's `invoke` doesn't yet forward the
// signal to the Rust command, so the *actual* backend cancellation
// (pool-pressure ≤1 per node_id) happens on the Rust side via the
// `DIFF_NODE_CANCEL` map — see `commands::diff::acquire_diff_cancel`.
// The frontend signal exists to (a) drop stale local results so the UI
// doesn't flicker, and (b) be the seam a future Tauri signal-aware
// invoke can plug into without touching call sites.
export const diffNodeAgainstBase = (
  nodeId: number,
  signal?: AbortSignal,
): Promise<DiffResult> => {
  if (signal?.aborted) {
    // Don't even kick off an IPC for a request the caller has already
    // superseded — a freshly aborted controller has nothing to wait
    // for, and starting the network round-trip would just produce a
    // promise we'd discard.
    return Promise.reject(new DOMException('aborted', 'AbortError'));
  }
  return _invoke<DiffResult>('diff_node_against_base', { nodeId });
};

export const diffNodeFileAgainstBase = (
  nodeId: number,
  filePath: string,
  signal?: AbortSignal,
): Promise<DiffResult> => {
  // Issue #1181 — see `diffNodeAgainstBase` for the rationale. Per-file
  // diffs share the same pool-pressure concern (the overlay's rapid
  // file-switching + `git-changed` bursts pile up the same way the
  // review panel does).
  if (signal?.aborted) {
    return Promise.reject(new DOMException('aborted', 'AbortError'));
  }
  return _invoke<DiffResult>('diff_node_file_against_base', { nodeId, filePath });
};

/** Lightweight base-relative file list for an Agent Node. The command
 * returns paths, statuses, and line counts without building or highlighting
 * hunks; the centre diff overlay loads a single file only after the user
 * chooses it. */
export const nodeChangedFiles = (nodeId: number) =>
  _invoke<GitStatus[]>('node_changed_files', { nodeId });

// File watcher
export const watchAgentNode = (nodeId: number) =>
  _invoke('watch_agent_node', { nodeId });

export const unwatchAgentNode = (nodeId: number) =>
  _invoke('unwatch_agent_node', { nodeId });

// File tree
export type { FileNode };

export const listDirectory = (path: string, maxDepth?: number) =>
  _invoke<FileNode>('list_directory', { path, maxDepth });

export const openInEditor = (path: string) =>
  _invoke('open_in_editor', { path });

export const openInFileManager = (path: string) =>
  _invoke('open_in_file_manager', { path });

export const getUserConfigDir = () =>
  _invoke<string>('get_user_config_dir');

// Git — `GitStatus` is generated from the Rust struct (issue #359).
// The Rust `status` field is a plain `String`, so the generated type widens
// the old `'added' | 'modified' | ...` union to `string`; consumers that
// switch on it still compare fine.
import type { GitStatus } from '../types/generated/GitStatus';
export type { GitStatus };

export const getGitStatus = (path: string) =>
  _invoke<GitStatus[]>('get_git_status', { path });

export type { GitBranchStatus };

export const getGitBranchStatus = (path: string) =>
  _invoke<GitBranchStatus | null>('get_git_branch_status', { path });

export type { GitSummary };

export const getGitSummary = (path: string) =>
  _invoke<GitSummary>('get_git_summary', { path });

// Issue #1374 — per-file quick actions for the center diff overlay's
// header. Both are repo-relative-path operations on the diff's `rootPath`.
export const stageFile = (repoPath: string, filePath: string) =>
  _invoke<void>('stage_file', { repoPath, filePath });

export const revertFile = (repoPath: string, filePath: string) =>
  _invoke<void>('revert_file', { repoPath, filePath });

export const getDefaultBranch = (path: string) =>
  _invoke<string>('get_default_branch', { path });

/** One-shot static snapshot for the git-status panel: repo-ness, GitHub
 *  auth, and the default branch. Replaces the three parallel IPCs
 *  (`check_is_git_repo` + `check_gh_auth` + `get_default_branch`) that
 *  `useMeshGitStatus` used to fan out (issue #348). */
export const getMeshGitStatic = (path: string) =>
  _invoke<MeshGitStatic>('get_mesh_git_static', { path });

export type { GitSyncResult };

export const gitSync = (path: string) =>
  _invoke<GitSyncResult>('git_sync', { path });

// ── Mesh health & recovery (issue #231) ─────────────────────────────────────

// Generated from the Rust structs in `src-tauri/src/models/mod.rs` (issue #404).
// Doc-comments from the hand-written interfaces now live on the Rust side and
// are picked up by the generated `.ts` files.
export type { HoldingWorktree, MeshHealth };

export const getMeshHealth = (meshId: number) =>
  _invoke<MeshHealth>('get_mesh_health', { meshId });

export type { RestoreResult };

export const restoreMeshToBase = (meshId: number) =>
  _invoke<RestoreResult>('restore_mesh_to_base', { meshId });

export type { FreeResult };

export const freeBaseBranch = (meshId: number, worktreePath: string) =>
  _invoke<FreeResult>('free_base_branch', { meshId, worktreePath });

// ── Git prune (branches & worktrees) ────────────────────────────────────────
//
// Generated from the Rust structs in `src-tauri/src/models/mod.rs` (issue #404).
export type { BranchInfo, WorktreeInfo, GitRepoPruneInfo };

export const getGitPruneInfo = (meshId: number) =>
  _invoke<GitRepoPruneInfo[]>('get_git_prune_info', { meshId });

export const deleteBranches = (meshId: number, worktreePath: string, branchNames: string[]) =>
  _invoke<void>('delete_branches', { meshId, worktreePath, branchNames });

export const deleteWorktrees = (worktreePaths: string[]) =>
  _invoke<void>('delete_worktrees', { worktreePaths });

// Issue #657: returns the trimmed `git fetch --prune` stderr so the
// frontend can surface git's own output (or an empty string on a no-op).
export const pruneRemoteTracking = (worktreePath: string) =>
  _invoke<string>('prune_remote_tracking', { worktreePath });

// ── Blocked worktree cleanup (issue #2139) ──────────────────────────────────
//
// Closing a node defers its worktree removal to a durable queue. When a removal
// cannot complete, the row keeps the failed operation, the OS error, the attempt
// count and the backoff deadline — these calls are what the blocked-cleanup
// dialog reads and acts on.

/** Every blocked worktree cleanup, with its persisted evidence. */
export const listPendingWorktreeRemovals = () =>
  _invoke<PendingWorktreeRemoval[]>('list_pending_worktree_removals');

/** Retry one cleanup now, ignoring its backoff. The status says what actually
 *  happened to the folder; only `removed` means it was deleted. */
export const retryWorktreeCleanup = (worktreePath: string) =>
  _invoke<WorktreeCleanupRetryResult>('retry_worktree_cleanup', { worktreePath });

/** "Keep worktree" — cancel the cleanup intent for one path. The reply says
 *  what was done to the disk and whether the intent was cancelled (false when
 *  the staged copy could not be moved back, so the entry stays queued). */
export const dismissWorktreeCleanup = (worktreePath: string) =>
  _invoke<WorktreeCleanupDismissalResult>('dismiss_worktree_cleanup', { worktreePath });

/** Which processes are pinning a worktree directory. Read-only; nothing is
 *  terminated. */
export const diagnoseWorktreeCleanupBlockers = (worktreePath: string) =>
  _invoke<BlockingProcess[]>('diagnose_worktree_cleanup_blockers', { worktreePath });

/** Explicitly terminate one process the diagnosis named. Only ever called from
 *  a user action on a row the diagnosis returned; the backend re-diagnoses and
 *  refuses a pid that is not a current blocker of `worktreePath`. */
export const releaseWorktreeCleanupBlocker = (worktreePath: string, pid: number) =>
  _invoke<void>('release_worktree_cleanup_blocker', { worktreePath, pid });

// Attention
export const registerAttentionNode = (nodeId: number) =>
  _invoke('register_attention_node', { nodeId });

export const clearAttentionNode = (nodeId: number) =>
  _invoke('clear_attention_node', { nodeId });

export const isAttentionPending = (nodeId: number) =>
  _invoke<boolean>('is_attention_pending', { nodeId });

// PR
export const createPr = (sessionId: number, title: string, body: string) =>
  _invoke<string>('create_pr', { sessionId, title, body });

export const mergePr = (prUrl: string, mergeMethod?: string) =>
  _invoke<string>('merge_pr', { prUrl, mergeMethod });

export const getCurrentBranch = (sessionId: number) =>
  _invoke<string>('get_current_branch', { sessionId });

export const checkGhAuth = () =>
  _invoke<boolean>('check_gh_auth');

/** Open PR summary for an agent node — surfaces as the "PR #N" chip.
 *  Generated from the Rust struct in `src-tauri/src/commands/pr.rs` (issue #404). */
export type { OpenPr };

export const getOpenPrForNode = (nodeId: number) =>
  _invoke<OpenPr | null>('get_open_pr_for_node', { nodeId });

/** Return the `https://github.com/{owner}/{repo}` web URL for a mesh's
 *  `origin` remote, or `null` when the origin isn't a GitHub URL (or the
 *  mesh has no origin at all). The return is intentionally a plain
 *  `string | null` — no generated wire type — so the IPC contract stays
 *  a single string (issue #359's "no hand-declared TS interface for a
 *  Rust wire type" rule is preserved trivially). Consumed by the mesh
 *  context menu's "View on GitHub" item and by the Issues / PRs probe
 *  headers' GitHub buttons. */
export const getGitHubUrlForMesh = (meshId: number) =>
  _invoke<string | null>('get_github_url_for_mesh', { meshId });

// GitHub Issues — `GitHubIssue` is generated from the Rust struct
// (src-tauri/src/commands/pr.rs) into src/types/generated/; see top import.
// Re-exported here so existing `import { GitHubIssue } from '../lib/tauri'`
// call sites keep working. Issue #359.
export type { GitHubIssue };

export const getRepoIssues = (meshId: number) =>
/// Returns the feed wrapper, not a bare array: a paginated read must be able to
/// say it is incomplete instead of letting the panel imply that page 1 is the
/// whole repository (issue #2024 rank 6 / #1528).
  _invoke<GitHubIssueFeed>('get_repo_issues', { meshId });

export const getRepoLabels = (meshId: number) =>
  _invoke<string[]>('get_repo_labels', { meshId });

export const setIssueLabel = (meshId: number, issueNumber: number, label: string, present: boolean) =>
  _invoke<void>('set_issue_label', { meshId, issueNumber, label, present });

// GitHub Pull Requests — `GitHubPullRequest` / `PrMergeability` are generated
// from the Rust structs (src-tauri/src/commands/pr.rs) into
// src/types/generated/; see top import. Re-exported here so the PR probe tab
// can `import { GitHubPullRequest } from '../lib/tauri'` alongside the issue
// types. Issue #359.
export type { GitHubPullRequest, PrMergeability, PrMergeabilityEntry, PrFileEntry };

/** List PRs for a mesh's repo, filtered by `state` (`'open'` or `'closed'`).
 * Issue #1529: cohesive summary query — list fields plus `mergeable` /
 * `mergeable_state` inline via the GraphQL summaries connection (O(pages)).
 * The panel consumes this single call and never orchestrates per-row
 * enrichment. */
export const getRepoPulls = (meshId: number, state: 'open' | 'closed') =>
  _invoke<GitHubPullRequestFeed>('get_repo_pulls', { meshId, state });

/// Per-PR mergeability enrichment — the `/pulls` list endpoint omits it, so
/// the panel fetches this once per open PR. `mergeable` is `null` while
/// GitHub is still computing the merge. **Deprecated on desktop** — use
/// [`getPrsMergeability`] for the batched call (issue #418); the per-PR
/// shape survives for the mobile HTTP route at
/// `GET /api/meshes/{id}/pulls/{n}/mergeability`.
export const getPrMergeability = (meshId: number, prNumber: number) =>
  _invoke<PrMergeability>('get_pr_mergeability', { meshId, prNumber });

/// Batched PR mergeability (issue #418, reimplemented O(pages) for #1529).
/// **Deprecated on desktop** — the panel now reads `mergeable` inline from
/// [`getRepoPulls`] and never calls this. Survives for backward compat
/// (older/mobile callers): one GraphQL summaries fetch serves the whole
/// batch, missing numbers become the `"error: …"` sentinel.
export const getPrsMergeability = (meshId: number, prNumbers: number[]) =>
  _invoke<PrMergeabilityEntry[]>('get_prs_mergeability', { meshId, prNumbers });

/// List the files changed in a single PR (issue #421). Backed by GitHub's
/// `/pulls/{n}/files` endpoint; one call returns the whole PR with each
/// file's unified-diff `patch`. The Center Diff Overlay parses the patch
/// line-by-line to render +/−/context rows. Distinct from `getRepoPulls` /
/// `getPrMergeability` because the panel needs the diff payload, not just
/// the metadata.
export const getPrFiles = (meshId: number, prNumber: number) =>
  _invoke<PrFileFeed>('get_pr_files', { meshId, prNumber });

export const spawnIssueAgent = (meshId: number, issueNumber: number, issueTitle: string, provider?: string) =>
  _invoke<AgentNode>('spawn_issue_agent', { meshId, issueNumber, issueTitle, provider });

/// Fast acceptance of an issue spawn. The backend commits the `pending` row,
/// starts the slow intent-driven launch in the background, and later emits
/// `node-spawn-completed` or `node-spawn-failed`. The returned draft keeps the
/// existing wire shape for compatibility; callers no longer need to hand the
/// transient prefill to a second IPC command.
export type { IssueNodeDraft };

export const createIssueNode = (meshId: number, issueNumber: number, issueTitle: string, provider?: string, configurationId?: string) =>
  _invoke<IssueNodeDraft>('create_issue_node', { meshId, issueNumber, issueTitle, provider, configurationId });

export const spawnHandoverAgent = (meshId: number, prefill: string, provider?: string) =>
  _invoke<AgentNode>('spawn_handover_agent', { meshId, prefill, provider });


export const createPrForMesh = (meshPath: string, title: string, body: string, baseBranch: string) =>
  _invoke<string>('create_pr_for_mesh', { meshPath, title, body, baseBranch });

/// One backend-owned acceptance call. The PR row's `+` button is on the
/// `SpawnButtonCluster`; the tab now relies on `create_pr_node` to
/// accept the row and start the intent-driven launch in the background.
///
/// The `headRef` field comes from the GitHub API's `head.ref` (now exposed
/// on `GitHubPullRequest` for this purpose). For fork PRs (issue #443) the
/// stage-2 path adds the fork as a remote (`fork-<login>`) and fetches the
/// head ref from there; the `headRepoOwner` + `headRepoCloneUrl` arguments
/// carry that info from the GitHub list response to the node row.
///
/// `headSha` (issue #444) is the PR's head commit SHA at click time, also
/// exposed on `GitHubPullRequest` via `head_sha`. The backend persists it
/// as `source_pr_pinned_sha` on the new node and verifies the local
/// `origin/<head_ref>` SHA matches it after `git fetch`, emitting a
/// non-fatal `pr_sha_drift` `mesh-sync-warning` on mismatch (force-push
/// / rebase between click-time and spawn-time). An empty `headSha` skips
/// the drift check (same fail-open semantics as `pr_head_unfetchable`).
///
/// Reuses the generated `IssueNodeDraft` type for the return value: the wire
/// shape is identical (flattened `AgentNode` + `prefill`), so no new TS
/// type is generated.
export const createPrNode = (
  meshId: number,
  prNumber: number,
  prTitle: string,
  headRef: string,
  headSha: string,
  provider?: string,
  headRepoOwner?: string,
  headRepoCloneUrl?: string,
  configurationId?: string,
  // Spawn a *reviewer sibling* for the PR (the PR pill's "Spawn reviewer
  // agent" row): a distinct name so it does not share the implementation
  // node's worktree — see `commands::agent::create_pr_node`'s `reviewer`
  // parameter for why sharing one is destructive. Omit for the probe's `+`.
  reviewer?: boolean,
) =>
  _invoke<IssueNodeDraft>('create_pr_node', {
    meshId,
    prNumber,
    prTitle,
    headRef,
    headSha,
    provider,
    headRepoOwner,
    headRepoCloneUrl,
    configurationId,
    reviewer,
  });

// AI context portability
export type { AiContextStatus };

export const detectAiContext = (meshPath: string) =>
  _invoke<AiContextStatus>('detect_ai_context', { meshPath });

export const createAiContextPortabilityPr = (meshId: number) =>
  _invoke<string>('create_ai_context_portability_pr', { meshId });

// (`listProviders` + `ProviderInfo` re-export moved to `./tauri/provider` —
// see the `export * from './tauri/provider'` line near the top of this file.)

// Agent Node Discovery — generated from the Rust struct (issue #359 + #490,
// re-exported here per #404 so call sites that import from `../lib/tauri`
// keep working).
export type { ArchivedAgentNode };

export const discoverAgentNodes = (meshId: number, meshPath: string) =>
  _invoke<ArchivedAgentNode[]>('discover_agent_nodes', { meshId, meshPath });

export const importDiscoveredAgentNode = (
  meshId: number,
  meshPath: string,
  cliSessionId: string,
  branch: string,
  worktreeName: string | null,
  provider?: string,
) =>
  _invoke<AgentNode>('import_discovered_agent_node', {
    meshId, meshPath, cliSessionId, branch, worktreeName, provider
  });

// ── App startup ────────────────────────────────────────────────────────────
//
// Re-attaches the in-process PTY for every node whose previous run was
// interrupted (status === 'suspended'). Returns the ids of the nodes that
// were actually resumed.
export const autoResumeAgentNodes = () =>
  _invoke<number[]>('auto_resume_agent_nodes');

// ── Paths & clipboard ──────────────────────────────────────────────────────
//
// `to_host_path` is a no-op for native paths and normalises WSL/Git-Bash
// variants — used by the OS file-drop paste path.
export const toHostPath = (path: string) =>
  _invoke<string>('to_host_path', { path });

/** Native clipboard read. On macOS this bypasses the WKWebView
 *  clipboard-permission popup by shelling to `pbpaste`; on other platforms it
 *  may reject, and callers fall back to `navigator.clipboard.readText()`. */
export const readClipboard = () =>
  _invoke<string>('read_clipboard');

/** The absolute log/profile locations this process resolved at startup
 *  (issue #1525). The backend opened these files before the database, so this
 *  is the only source that can tell a user where their log actually is. */
export const getDiagnosticPaths = () =>
  _invoke<DiagnosticPaths>('get_diagnostic_paths');

// ── Agent PTY transport ────────────────────────────────────────────────────
//
// `writeToAgent` is declared above next to the other agent IPCs; `resizeAgent`
// here completes the PTY-side surface used by `TerminalRegistry`. It rejects
// with the string `'Agent not running'` while the PTY isn't up yet — callers
// match on that exact value to ignore the expected race (see
// `TerminalRegistry.syncPtySize`).
export const resizeAgent = (sessionId: number, rows: number, cols: number) =>
  _invoke('resize_agent', { sessionId, rows, cols });

/**
 * Coerce a Tauri Channel raw-binary message into a `Uint8Array`.
 *
 * Tauri 2's Channel delivers `InvokeResponseBody::Raw` as an
 * `ArrayBuffer` on the eval fast path (<1 KiB), or a `Response` on the
 * fetch path for larger frames. Tests and older runtimes may hand us a
 * `Uint8Array` or another
 * `ArrayBufferView` directly. Anything else is ignored; the JSON
 * `agent-output` event is retained only for test injection.
 */
export function bytesFromChannelMessage(message: unknown): Uint8Array | null {
  if (message instanceof Uint8Array) return message;
  if (message instanceof ArrayBuffer) return new Uint8Array(message);
  if (ArrayBuffer.isView(message)) {
    const view = message as ArrayBufferView;
    return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
  }
  return null;
}

interface ChannelResponse {
  arrayBuffer: () => Promise<ArrayBuffer>;
}

function isChannelResponse(message: unknown): message is ChannelResponse {
  if (typeof message !== 'object' || message === null) return false;
  return typeof (message as { arrayBuffer?: unknown }).arrayBuffer === 'function';
}

async function bytesFromChannelMessageAsync(message: unknown): Promise<Uint8Array | null> {
  const bytes = bytesFromChannelMessage(message);
  if (bytes) return bytes;
  if (!isChannelResponse(message)) return null;
  return new Uint8Array(await message.arrayBuffer());
}

type PtyOutputCommand = 'subscribe_agent_output' | 'subscribe_build_run_output';

/**
 * Shared Channel subscriber for raw PTY bytes (issues #1385 / #1393).
 * Small frames arrive as an `ArrayBuffer`; Tauri delivers larger frames
 * as a fetch `Response` whose body must be read asynchronously.
 * No-ops in tests whose `@tauri-apps/api/core` mock omits `Channel` —
 * those suites keep using the JSON event fallback.
 */
function subscribeRawPtyOutput(
  command: PtyOutputCommand,
  sessionId: number,
  onChunk: (data: Uint8Array) => void,
): Promise<void> {
  if (typeof Channel !== 'function') return Promise.resolve();
  const onChunkChannel = new Channel<ArrayBuffer | Uint8Array | ChannelResponse>();
  // Tauri's large-raw-payload path hands the callback a Response. Its body
  // read is asynchronous, so serialize decoding to preserve the Channel's
  // byte order when multiple fetches are in flight.
  let decodeQueue: Promise<void> = Promise.resolve();
  let queuedFrames = 0;
  onChunkChannel.onmessage = (message) => {
    const directBytes = bytesFromChannelMessage(message);
    // Keep the small raw-frame fast path synchronous. Apart from avoiding an
    // extra microtask for keystroke echoes, this preserves the existing
    // interactive latency contract. Once a Response is queued, subsequent
    // direct frames join the queue so they cannot overtake it.
    // The sync path still has to mirror the queued path's error handling --
    // a throwing onChunk here would otherwise escape unhandled (the queued
    // .catch would swallow it).
    if (directBytes && queuedFrames === 0) {
      try {
        onChunk(directBytes);
      } catch (error) {
        console.error(`[PTY] failed to decode ${command} Channel frame:`, error);
      }
      return;
    }
    queuedFrames++;
    decodeQueue = decodeQueue
      .then(async () => {
        const bytes = directBytes ?? await bytesFromChannelMessageAsync(message);
        if (bytes) onChunk(bytes);
      })
      .catch((error) => {
        console.error(`[PTY] failed to decode ${command} Channel frame:`, error);
      })
      .finally(() => {
        queuedFrames--;
      });
  };
  return _invoke(command, { sessionId, onChunk: onChunkChannel });
}

/**
 * Subscribe this webview to raw agent PTY bytes for `sessionId` (issue #1385).
 * Production output skips Base64+JSON.
 */
export const subscribeAgentOutput = (
  sessionId: number,
  onChunk: (data: Uint8Array) => void,
): Promise<void> => subscribeRawPtyOutput('subscribe_agent_output', sessionId, onChunk);

/** Drop the binary Channel registered by [`subscribeAgentOutput`]. Idempotent. */
export const unsubscribeAgentOutput = (sessionId: number) =>
  _invoke('unsubscribe_agent_output', { sessionId });

/**
 * Subscribe this webview to raw Build/Run PTY bytes for `sessionId`
 * (issue #1393). Same Channel transport as [`subscribeAgentOutput`].
 */
export const subscribeBuildRunOutput = (
  sessionId: number,
  onChunk: (data: Uint8Array) => void,
): Promise<void> => subscribeRawPtyOutput('subscribe_build_run_output', sessionId, onChunk);

/** Drop the binary Channel registered by [`subscribeBuildRunOutput`]. Idempotent. */
export const unsubscribeBuildRunOutput = (sessionId: number) =>
  _invoke('unsubscribe_build_run_output', { sessionId });

/** Reply to a remote-pane snapshot request from the HTTP server. The pair
 *  (`request_id`, `data`) is matched against an in-flight promise on the
 *  backend; the call returns immediately and has no result. */
export const submitTerminalSnapshot = (requestId: string, data: string) =>
  _invoke<void>('submit_terminal_snapshot', { requestId, data });

// ── Build/Run side-panel PTY ───────────────────────────────────────────────
//
// Separate PTY surface from the agent terminal — keeps a long-running build
// or `npm run dev` independent of the agent's PTY lifecycle. `build_run`
// returns once the child has been spawned; production output flows via
// `subscribeBuildRunOutput`. The `build-run-output-<nodeId>` event is the
// test-injection fallback.
export const buildRun = (nodeId: number, mode: 'build' | 'run' | 'terminal') =>
  _invoke('build_run', { nodeId, mode });

export const closeBuildRun = (nodeId: number) =>
  _invoke('close_build_run', { nodeId });

export const writeToBuildRun = (nodeId: number, data: string) =>
  _invoke('write_to_build_run', { nodeId, data });

export const resizeBuildRun = (nodeId: number, rows: number, cols: number) =>
  _invoke('resize_build_run', { nodeId, rows, cols });

// ── App-wide preferences (`preferences.json`) ──────────────────────────────
//
// Generated from `crate::preferences::AppPreferences` (issue #404). The
// `google_cloud_project` field is included to match the Rust struct in full
// even though the current settings UI only reads two fields.
export type { AppPreferences };
export type { CorruptionInfo, CorruptionReason, PreferencesHealth, PreferencesStatus, RecoveryOutcome };

export const getAppPreferences = () =>
  _invoke<AppPreferences>('get_app_preferences');

// ── Corrupt-file recovery (issue #1523) ───────────────────────────────────
//
// `get_app_preferences` still returns defaults for an unreadable file so
// every read-only surface keeps working; `get_preferences_health` is the
// separate, *successful* call that says whether those defaults are real
// settings or a stand-in. Keeping it a distinct command is deliberate —
// folding corruption into an `Err` would force the UI to classify the
// failure by matching error prose.

export const getPreferencesHealth = () =>
  _invoke<PreferencesHealth>('get_preferences_health');

/** Put the last-known-good backup back over a corrupt file, archiving
 *  whatever is on disk first. The non-destructive recovery. */
export const restorePreferencesBackup = () =>
  _invoke<RecoveryOutcome>('restore_preferences_backup');

/** Archive the current file and start from defaults. Destructive by design:
 *  the caller must confirm it, and the returned `archive_path` says where the
 *  original bytes went. */
export const resetAppPreferences = () =>
  _invoke<RecoveryOutcome>('reset_app_preferences');

/** Open the folder holding `preferences.json` in the OS file manager. The
 *  directory, not the file — a file manager hands a `.json` to whatever
 *  application claims the extension, which launches an editor instead of
 *  showing the folder. */
export const openPreferencesLocation = () =>
  _invoke('open_preferences_location');

/** Pass `null` (or an empty string, which the backend filters out) to clear
 *  the override and fall back to the hardcoded `anthropic` default. */
export const setAppDefaultProvider = (provider: string | null) =>
  _invoke('set_app_default_provider', { provider }).then((result) => {
    // The app-wide default feeds every mesh's resolution — evict all cached
    // defaults and notify open surfaces, mirroring updateMeshColumn above.
    clearDefaultProviderPromises();
    void emit(PROVIDER_LIST_CHANGED_EVENT).catch(() => {});
    return result;
  });

/** App-wide reviewer Spawn Option. `null` restores the source-agent fallback;
 * this is separate from the ordinary default provider so adversarial reviews
 * can use an independent harness. */
export const setAppReviewerProvider = (provider: string | null) =>
  _invoke('set_app_reviewer_provider', { provider });

/** Issue #824: pick the backend that summarises PTY output into a slug.
 *  Pass `null` (or empty) to **disable** auto-naming — nodes keep their
 *  random `adjective-adjective-noun` slugs until the user picks a value
 *  in Settings → Auto-naming. Distinct from `default_provider`: a
 *  rename runs frequently on trivial content, so the user opts in
 *  explicitly rather than inheriting whatever expensive tier the
 *  spawned node happens to be on. */
export const setAppNamingProvider = (provider: string | null) =>
  _invoke('set_app_naming_provider', { provider });

/** App-wide Circuit agent pool cap (`null` = uncapped, `0` = pause new spawns).
 *  Semantics documented on `AppPreferences::circuit_agent_pool_size`. */
export const setAppCircuitAgentPoolSize = (size: number | null) =>
  _invoke('set_app_circuit_agent_pool_size', { size });

/** Whether to confirm before quitting with active agent sessions (issue #1501).
 *  `true` (default) surfaces the exit-confirmation modal; `false` closes
 *  without friction. */
export const setAppConfirmBeforeQuit = (confirm: boolean) =>
  _invoke('set_app_confirm_before_quit', { confirm });

/** Retract a vetoed window close (issue #1501).
 *  Clears the backend's eager expected-exit marking (`USER_CLOSE_REQUESTED`
 *  + watchdog marker) when the user backs out of the exit-confirmation
 *  modal, so a later real crash still auto-relaunches. */
export const cancelWindowClose = () =>
  _invoke('cancel_window_close');

/** Confirmed exit (issue #1501): backend lifecycle shutdown for the
 *  exit-confirmation modal. A custom command, not a window IPC — window
 *  commands are ACL-gated and the modal must not depend on the
 *  compiled-in capability set. */
export const exitApplication = () =>
  _invoke('exit_application');

/** Native Windows Snap Layouts for the bespoke title bar (ADR-0035).
 *
 *  The window controls can't get the Windows 11 snap flyout from CSS or JS:
 *  the shell only offers it to a window whose `WM_NCHITTEST` answers
 *  `HTMAXBUTTON`, and our page lives in a WebView2 child HWND that answers the
 *  hit test first. A native child window is parked over the maximise button
 *  instead, and this reports that button's real box so it lands exactly on top
 *  of it.
 *
 *  Logical (CSS) pixels — the backend applies the window DPI scale, which the
 *  frontend cannot see. Reported on mount and again on resize; the backend is
 *  idempotent. No-op off Windows. */
export const setTitlebarMaximizeMetrics = (metrics: {
  rightInset: number;
  top: number;
  width: number;
  height: number;
}) => _invoke('set_titlebar_maximize_metrics', { ...metrics });

/** Buildmesh-wide default Worktree Node directory (issue #1519).
 *  Pass `null` (or blank) to clear and restore `.claude/worktrees` under
 *  each inheriting Mesh root. Relative resolves from each Mesh root;
 *  absolute stored verbatim (per-Mesh env validation at resolution).
 *  Triggers a background rebuild of inheriting pools; live nodes keep
 *  their persisted `worktree_path`. */
export const setAppWorktreeDirectory = (directory: string | null) =>
  _invoke('set_app_worktree_directory', { directory });

/** Built-in probe-spawn prompt templates (placeholders unrendered),
 *  shown in Settings as the defaults. Generated from
 *  commands::preferences::ProbeSpawnPromptDefaults.*/
export type { ProbeSpawnPromptDefaults };

export const getProbeSpawnPromptDefaults = () =>
  _invoke<ProbeSpawnPromptDefaults>('get_probe_spawn_prompt_defaults');

/** Custom template for the initial prompt of agents spawned from the
 *  Probe's GitHub Issues tab. Pass `null` (or blank, which the backend
 *  collapses) to clear the override and restore the built-in wording.*/
export const setAppIssueSpawnPrompt = (prompt: string | null) =>
  _invoke('set_app_issue_spawn_prompt', { prompt });

/** Custom template for the initial prompt of agents spawned from the
 *  Probe's Pull Requests tab. Pass `null` (or blank) to clear and
 *  restore the built-in wording.*/
export const setAppPrSpawnPrompt = (prompt: string | null) =>
  _invoke('set_app_pr_spawn_prompt', { prompt });

// ── Application-level Agent Harness defaults + per-Mesh overrides + ────────
//    proxied-provider pairings + usage meters (issue #1150 / #1148 / #1151 /
//    ADR-0025 / #574 / #1680).
//
// ALL of these wrappers now live in `./tauri/provider` (issue #1656 Phase 2 —
// first facet). The `export * from './tauri/provider'` near the top of this
// file keeps every existing consumer compiling unchanged. New code should
// import directly from `./tauri/provider` so the import surfaces the domain.

export const setMinimaxApiKey = (key: string | null) =>
  _invoke('set_minimax_api_key', { key });

// ── Coordinator read API control (ADR-0008) ────────────────────────────────
//
// Generated from `commands::coordinator::CoordinatorStatus` (issue #404).
// `has_token` reports presence without ever leaking the token value — the
// token is only ever surfaced once, by `generateCoordinatorReadToken`.
export type { CoordinatorStatus };

export const getCoordinatorStatus = () =>
  _invoke<CoordinatorStatus>('get_coordinator_status');

export const setCoordinatorApiEnabled = (enabled: boolean) =>
  _invoke('set_coordinator_api_enabled', { enabled });

/** Mint (or replace) the read-scoped token and return it for the user to copy.
 *  Replacing invalidates the previously issued token; the value is shown once. */
export const generateCoordinatorReadToken = () =>
  _invoke<string>('generate_coordinator_read_token');

// ── Authorized devices (issue #502) ────────────────────────────────────────
//
// Per-device session tokens minted at mobile pairing. The list omits the token
// hash (the secret never crosses IPC); revoking deletes the device and kicks any
// live socket it holds, so revocation takes effect immediately.
export type { DeviceSession };

export const listDeviceSessions = () =>
  _invoke<DeviceSession[]>('list_device_sessions');

export const revokeDeviceSession = (id: number) =>
  _invoke('revoke_device_session', { id });

// ── OpenCode Console OAuth (issue #956 + #969) ────────────────────────────
//
// Drive the RFC 8628 Device Flow + post-dance workspace enumeration from the
// Settings → Providers tab. Stateless-server design: React holds the dance
// state (`device_code`, `intervalSecs`, `startedAtMs`); each call is one
// round-trip. `revoke_opencode_console` is idempotent — the card's "Sign
// out" affordance never errors on a no-op (mirrors `windows_cred::delete`).
// All four commands are wired in `lib.rs:566-575` of this branch.
import type { OpenCodeWorkspace } from '../types/generated/OpenCodeWorkspace';
import type { OpenCodeDeviceFlowStart } from '../types/generated/OpenCodeDeviceFlowStart';
import type { OpenCodeDeviceCodeStatus } from '../types/generated/OpenCodeDeviceCodeStatus';
import type { OpenCodeTokenResponse } from '../types/generated/OpenCodeTokenResponse';
import type { OpenCodeConsoleStatus } from '../types/generated/OpenCodeConsoleStatus';
export type {
  OpenCodeWorkspace,
  OpenCodeDeviceFlowStart,
  OpenCodeDeviceCodeStatus,
  OpenCodeTokenResponse,
  OpenCodeConsoleStatus,
};

export const startOpencodeDeviceFlowConsole = () =>
  _invoke<OpenCodeDeviceFlowStart>('start_device_flow_console');

export const pollOpencodeDeviceToken = (
  deviceCode: string,
  currentIntervalSecs: number,
  // Renamed from `expiresInSecs` for issue #1010: this is the ORIGINAL
  // window length captured at dance-start, NOT a per-tick countdown.
  // The Rust gate `now_ms - started_at_ms >= original_expires_in_secs*1000`
  // must stay monotonic across the full window.
  originalExpiresInSecs: number,
  startedAtMs: number,
) =>
  _invoke<OpenCodeDeviceCodeStatus>('poll_opencode_device_token', {
    deviceCode,
    currentIntervalSecs,
    originalExpiresInSecs,
    startedAtMs,
  });

export const listOpencodeWorkspaces = (accessToken?: string) =>
  _invoke<OpenCodeWorkspace[]>('list_opencode_workspaces', { accessToken });

export const persistOpencodeTokens = (
  token: OpenCodeTokenResponse,
  workspaceId?: string,
  serverId?: string,
) =>
  _invoke<void>('persist_opencode_tokens', {
    token,
    workspaceId,
    serverId,
  });

export const revokeOpencodeConsole = () =>
  _invoke<void>('revoke_opencode_console');

// Read-only session state for the Settings → OpenCode Console card.
// Returns `signed_in: true` plus the workspace picker list, the
// active workspace id, the access-token expiry epoch (in ms), and a
// `session_expired` flag when the credential's `expires_at` is in
// the past. Consumed by `OpenCodeAccountCard` on mount to render
// `signedIn` without re-running the dance. See
// `services::opencode_oauth::OpenCodeConsoleStatus` for the Rust
// side; ts-rs export lives at `src/types/generated/OpenCodeConsoleStatus.ts`.
// Re-exported at the top of the OpenCode block alongside the other
// `OpenCode*` types so the type is in scope for `_invoke<OpenCodeConsoleStatus>(…)`.
export const getOpencodeConsoleStatus = () =>
  _invoke<OpenCodeConsoleStatus>('get_opencode_console_status');

// Persist a workspace switch without rotating the bearer. The Rust
// side re-writes the credential blob with a new `workspace_id` and
// keeps `access_token` / `refresh_token` / `expires_at` / `server_id`
// verbatim. On success, `opencode-console-changed` is emitted so the
// Usage tab re-fetches the live probe with `force=true`. The dropdown
// in `OpenCodeAccountCard` is the only caller today.
export const setOpencodeConsoleWorkspace = (workspaceId: string) =>
  _invoke<void>('set_opencode_console_workspace', { workspaceId });

// ── LAN/VPN exposure & self-signed TLS (issue #501) ────────────────────────
//
// Off by default: the server binds loopback only. Enabling exposure binds the
// machine's LAN interfaces over self-signed TLS (HTTPS/WSS) and rebinds live —
// no app restart. `getNetworkStatus` reports the switch and the bound port;
// `RealizedBind` (issue #586) is one element of its realized-listener list,
// used by the Settings UI to show *actual* exposure rather than just DB intent.
export type { NetworkStatus, RealizedBind };

export const getNetworkStatus = () =>
  _invoke<NetworkStatus>('get_network_status');

export const setLanExposureEnabled = (enabled: boolean) =>
  _invoke('set_lan_exposure_enabled', { enabled });

// ── Remote access (mobile QR) ──────────────────────────────────────────────
export const getLocalIp = () =>
  _invoke<string>('get_local_ip');

export const getRootToken = () =>
  _invoke<string>('get_root_token');

export const createPairingTicket = () =>
  _invoke<string>('create_pairing_ticket');

// Cert status (issue #635). The QR modal surfaces the server's current root
// fingerprint so a user whose installed root CA is stale can see the mismatch
// and re-install. Only the desktop reads `cert_path` (the HTTP route omits it
// to avoid leaking the Windows username across the LAN).
import type { CertChainStatus } from '../types/generated/CertChainStatus';
export type { CertChainStatus };

export const getCertChainStatus = () =>
  _invoke<CertChainStatus>('get_cert_chain_status');

/** Explicit "Reset trusted certificates" action (issue #1527). Wipes the
 *  persisted root + leaf + SAN sidecar + generation counter so the next
 *  bind mints a fresh root. The user's phone loses trust and must
 *  re-install via the install-QR. Returns the new `root_generation` so
 *  the UI can confirm the rotation took and re-fetch cert_status. The
 *  backend command also invalidates the live `TlsAcceptor` cache and
 *  triggers a rebind, so the iOS `.mobileconfig` QR (which the desktop
 *  re-mints after a reset) actually chains to the cert the listener is
 *  now serving.
 *
 *  **Idempotent**: calling on an already-empty tls/ dir is a no-op
 *  generation bump. Frontend exposes this as a Settings affordance;
 *  never auto-invoked from network-change paths.
 */
export const resetTrustedCertificates = () =>
  _invoke<number>('reset_trusted_certificates');

/** Root CA bytes for the phone-install QR (issue #702). Returns base64
 *  (standard alphabet, '=' padding) — concatenate with the data: prefix
 *  to produce the OS-installable URL. The desktop modal embeds this in
 *  a second QR; scanning the QR on Android/iOS routes through the OS
 *  CA installer instead of opening /install-cert.der in the desktop's
 *  WebView2. */
export const getRootCertDer = () =>
  _invoke<string>('get_root_cert_der');

/** Signed `.mobileconfig` profile for the iOS install-QR (issue #713).
 *  Returns base64 of a DER-encoded PKCS#7/CMS SignedData wrapping the
 *  unsigned Apple Configurator 2 plist — the same wire format as
 *  `openssl cms -sign -binary -outform DER -nodetach`. The frontend
 *  concatenates `data:application/x-apple-aspen-config;base64,` to
 *  produce the data: URL Safari intercepts on iOS ≥ 14. Sibling to
 *  `getRootCertDer` (the Android path) — kept as a separate command
 *  rather than a parameter so the failure surfaces cleanly per platform
 *  and the modal can hide the iOS tab on its own rejection without
 *  affecting the Android one. */
export const getRootCertMobileconfig = () =>
  _invoke<string>('get_root_cert_mobileconfig');

/** App-level metadata (issue #826). The updater guard in `lib/updater.ts`
 *  uses this to reject the dev profile (`*.dev` bundle id) from polling
 *  the stable release feed — `tauri:build:dev` is a production-mode Vite
 *  build, so an `import.meta.env.PROD` check alone can't tell them apart. */
export const getAppIdentifier = () =>
  _invoke<string>('get_app_identifier');

// (`__resetProviderCachesForTests` moved to `./tauri/provider` — issue #1656
// Phase 2 — first facet. The `export * from './tauri/provider'` near the
// top of this file re-exports it so existing tests continue to work.)

// ── Autopilot Circuits (spec #1205 / walking skeleton #1206) ─────────────
import type { AutopilotCircuit } from '../types/generated/AutopilotCircuit';
import type { CircuitAgentOwnership } from '../types/generated/CircuitAgentOwnership';
import type { CircuitRunDetail } from '../types/generated/CircuitRunDetail';
import type { CircuitWithRuns } from '../types/generated/CircuitWithRuns';
import type { CircuitProbeSnapshot } from '../types/generated/CircuitProbeSnapshot';
import type { CircuitQueueDirection } from '../types/generated/CircuitQueueDirection';
import type { CircuitQueueEntry } from '../types/generated/CircuitQueueEntry';
// Milestone 4 (#1209): the canvas editor consumes the blueprint AST, so
// the graph wire types ride the same sanctioned surface.
export type {
  AutopilotCircuit,
  CircuitAgentOwnership,
  CircuitQueueDirection,
  CircuitQueueEntry,
  CircuitRunDetail,
  CircuitWithRuns,
  CircuitProbeSnapshot,
};
export type { CircuitGraph } from '../types/generated/CircuitGraph';
export type { CircuitNode } from '../types/generated/CircuitNode';
export type { CircuitNodeKind } from '../types/generated/CircuitNodeKind';
export type { CircuitBlueprintKind } from '../types/generated/CircuitBlueprintKind';
export type { CircuitEdge } from '../types/generated/CircuitEdge';
export type { EdgeCondition } from '../types/generated/EdgeCondition';
export type { StepOutcome } from '../types/generated/StepOutcome';
export type { RunState } from '../types/generated/RunState';
export type { StepStatus } from '../types/generated/StepStatus';
export type { CapacityBind } from '../types/generated/CapacityBind';
export type { GithubActionKind } from '../types/generated/GithubActionKind';
export type { OpenPrPolicy } from '../types/generated/OpenPrPolicy';
export type { SessionStatusKind } from '../types/generated/SessionStatusKind';

export const listCircuits = (meshId: number) =>
  _invoke<AutopilotCircuit[]>('list_circuits', { meshId });

export const listCircuitAgentOwnerships = () =>
  _invoke<CircuitAgentOwnership[]>('list_circuit_agent_ownerships');

/** One circuit row — the canvas editor overlay's load unit (#1209). */
export const getCircuit = (circuitId: number) =>
  _invoke<AutopilotCircuit>('get_circuit', { circuitId });

/** Batched single-IPC load for the Probe tab: every circuit on the mesh with
 *  all active runs plus up to `limit` newest terminal runs (steps included). */
export const listCircuitsWithRuns = (meshId: number, limit?: number) =>
  _invoke<CircuitWithRuns[]>('list_circuits_with_runs', { meshId, limit });

/** Single-IPC hydration for the Circuits Probe ledger and mesh queue. */
export const listCircuitProbe = (meshId: number, limit?: number) =>
  _invoke<CircuitProbeSnapshot>('list_circuit_probe', { meshId, limit });

export const listCircuitQueue = (meshId: number) =>
  _invoke<CircuitQueueEntry[]>('list_circuit_queue', { meshId });

/** Creates a circuit with the canonical server-side blueprint:
 *  <trigger> → SpawnAgentNode (fresh) → InjectPty(prompt) → Notify.
 *  Trigger vocabulary (issue #1208): manual (default), interval,
 *  github_issue_label, github_pr_label. */
export type { CircuitTriggerKind } from '../types/generated/CircuitTriggerKind';
import type { CircuitTriggerKind } from '../types/generated/CircuitTriggerKind';
import type { CircuitBlueprintKind } from '../types/generated/CircuitBlueprintKind';

export const createCircuit = (
  meshId: number,
  name: string,
  description: string,
  initialPrompt: string,
  triggerKind: CircuitTriggerKind = 'manual',
  triggerLabel?: string,
  intervalSeconds?: number,
  blueprint: CircuitBlueprintKind = 'walking_skeleton'
) =>
  _invoke<AutopilotCircuit>('create_circuit', {
    meshId,
    name,
    description,
    initialPrompt,
    triggerKind,
    triggerLabel: triggerLabel ?? null,
    intervalSeconds: intervalSeconds ?? null,
    blueprint,
  });

export const setCircuitEnabled = (circuitId: number, enabled: boolean) =>
  _invoke<void>('set_circuit_enabled', { circuitId, enabled });

/** Canvas editor save seam (issue #1209): replace the whole blueprint
 *  AST. The backend validates the JSON parses before persisting. */
export const updateCircuitGraph = (circuitId: number, graphJson: string) =>
  _invoke<void>('update_circuit_graph', { circuitId, graphJson });

export const deleteCircuit = (circuitId: number) =>
  _invoke<void>('delete_circuit', { circuitId });

export const cancelCircuitRun = (runId: number) =>
  _invoke<void>('cancel_circuit_run', { runId });

export const moveCircuitRun = (runId: number, direction: CircuitQueueDirection) =>
  _invoke<void>('move_circuit_run', { runId, direction });

/** Persist an explicit front-to-back queue order (drag-drop / keyboard reorder). */
export const reorderCircuitQueue = (meshId: number, orderedRunIds: number[]) =>
  _invoke<void>('reorder_circuit_queue', { meshId, orderedRunIds });

/** Bulk cancel through the single-run cleanup path (one event per run). */
export const cancelCircuitRuns = (runIds: number[]) =>
  _invoke<void>('cancel_circuit_runs', { runIds });

export const triggerCircuitNow = (circuitId: number) =>
  _invoke<number>('trigger_circuit_now', { circuitId });

export const triggerCircuitFromNode = (
  nodeId: number,
  circuitId: number | null,
  maxRounds: number,
  reviewerProvider: string | null = null,
) =>
  _invoke<number>('trigger_circuit_from_node', { nodeId, circuitId, maxRounds, reviewerProvider });

/** Graceful pause: the graph stops advancing; current steps finish (#1207). */
export const pauseCircuitRun = (runId: number) =>
  _invoke<void>('pause_circuit_run', { runId });

/** Resume a paused run where it stopped (#1207). */
export const continueCircuitReview = (runId: number, additionalRounds = 1) =>
  _invoke<number>('continue_circuit_review', { runId, additionalRounds });

export const resumeCircuitRun = (runId: number) =>
  _invoke<void>('resume_circuit_run', { runId });

/** Approve a CollaboratorCheck gate parked in `blocked` (#1207). */
export const approveCircuitStep = (runId: number, nodeId: string) =>
  _invoke<void>('approve_circuit_step', { runId, nodeId });

export const listCircuitRuns = (circuitId: number, limit?: number) =>
  _invoke<CircuitRunDetail[]>('list_circuit_runs', { circuitId, limit });

/** Select a host-native Claude Code or native Codex configuration for Circuit classifiers. */
export const setCircuitClassifierProvider = (provider: string | null) =>
  _invoke<void>('set_circuit_classifier_provider', { provider });

export const listSemanticTurns = () =>
  _invoke<import('../types/generated/SemanticTurnPayload').SemanticTurnPayload[]>('list_semantic_turns');
