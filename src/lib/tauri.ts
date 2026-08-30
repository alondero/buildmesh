import { invoke as _rawInvoke } from '@tauri-apps/api/core';
import { logFrontend } from './frontendLog';
import { shapeArgs } from './ipcShape';
import type { AgentNode } from '../stores/agentNodeStore';
import type { Mesh } from '../stores/meshStore';
import type { AiContextStatus } from '../types/generated/AiContextStatus';
import type { AppPreferences } from '../types/generated/AppPreferences';
import type { AutopilotMode } from '../types/generated/AutopilotMode';
import type { AutopilotCompatibility } from '../types/generated/AutopilotCompatibility';
import type { AutopilotRunStateRow } from '../types/generated/AutopilotRunState';
import type { BillingBalance } from '../types/generated/BillingBalance';
import type { BillingMode } from '../types/generated/BillingMode';
import type { BranchInfo } from '../types/generated/BranchInfo';
import type { CoordinatorStatus } from '../types/generated/CoordinatorStatus';
import type { DeviceSession } from '../types/generated/DeviceSession';
import type { EnvType } from '../types/generated/EnvType';
import type { DiffHunk } from '../types/generated/DiffHunk';
import type { DiffLine } from '../types/generated/DiffLine';
import type { DiffResult } from '../types/generated/DiffResult';
import type { ArchivedAgentNode } from '../types/generated/ArchivedAgentNode';
import type { FileDiff } from '../types/generated/FileDiff';
import type { FileNode } from '../types/generated/FileNode';
import type { FreeResult } from '../types/generated/FreeResult';
import type { GitBranchStatus } from '../types/generated/GitBranchStatus';
import type { HarnessConfigValue } from '../types/generated/HarnessConfigValue';
import type { GitHubIssue } from '../types/generated/GitHubIssue';
import type { GitHubPullRequest } from '../types/generated/GitHubPullRequest';
import type { GitRepoPruneInfo } from '../types/generated/GitRepoPruneInfo';
import type { GitSummary } from '../types/generated/GitSummary';
import type { GitSyncResult } from '../types/generated/GitSyncResult';
import type { HoldingWorktree } from '../types/generated/HoldingWorktree';
import type { IssueNodeDraft } from '../types/generated/IssueNodeDraft';
import type { MeshRow } from '../types/generated/MeshRow';
import type { LoopStatusDto } from '../types/generated/LoopStatus';
import type { MeshGitStatic } from '../types/generated/MeshGitStatic';
import type { MeshHealth } from '../types/generated/MeshHealth';
import type { NetworkStatus } from '../types/generated/NetworkStatus';
import type { PickedFolder } from '../types/generated/PickedFolder';
import type { OpenPr } from '../types/generated/OpenPr';
import type { PrMergeability } from '../types/generated/PrMergeability';
import type { PrMergeabilityEntry } from '../types/generated/PrMergeabilityEntry';
import type { PrFileEntry } from '../types/generated/PrFileEntry';
import type { ApiSurface } from '../types/generated/ApiSurface';
import type { ModelTiers } from '../types/generated/ModelTiers';
import type { ProviderAccount } from '../types/generated/ProviderAccount';
import type { ProviderInfo } from '../types/generated/ProviderInfo';
import type { ProviderPairing } from '../types/generated/ProviderPairing';
import type { PairingVerification } from '../types/generated/PairingVerification';
import type { ProviderMeters } from '../types/generated/ProviderMeters';
import type { ProviderUsage } from '../types/generated/ProviderUsage';
import type { RealizedBind } from '../types/generated/RealizedBind';
import type { RestoreResult } from '../types/generated/RestoreResult';
import type { UsageWindow } from '../types/generated/UsageWindow';
import type { WorktreeInfo } from '../types/generated/WorktreeInfo';
import type { WorktreeCloseSafety } from './worktreeClose';
import {
  deleteDefaultProviderPromise,
  getDefaultProviderPromise,
  getProviderListPromise,
  resetProviderCachesForTests,
  setDefaultProviderPromise,
  setProviderListPromise,
} from './providerCache';

/**
 * Central IPC chokepoint (issue #386). Every wrapper below calls through
 * `_invoke`, which delegates to Tauri's `invoke` and — on rejection —
 * forwards a single `frontendLog` entry (command name + sanitized arg
 * shape) and re-throws the original error. The re-throw preserves every
 * existing call-site behaviour (stores set error state, components show
 * toasts, the bridge still sees the original `Error` for `window.error` /
 * `unhandledrejection` plumbing). The arg shape is the sanitized form
 * produced by `ipcShape.shapeArgs` so PII (API keys) and unbounded
 * payloads (terminal scrollback) are never written to `buildmesh.log`.
 *
 * Tests stub `_rawInvoke` (the aliased Tauri binding) via
 * `vi.mock('@tauri-apps/api/core', …)` and mock `frontendLog.logFrontend`
 * to assert on the formatted shape — see `tests/unit/ipc-error-logging.test.ts`.
 */
// Function declaration (not const) so it hoists — the wrappers below
// reference `_invoke` and the function name is referenced before its
// declaration in source order.
const ERROR_TEXT_CAP = 200;

async function _invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    // Only pass `args` when defined so the call shape matches the
    // pre-wrapper `invoke('cmd')` form (some tests assert on
    // argument count, e.g. `toHaveBeenCalledWith('list_meshes')`).
    return args === undefined
      ? await _rawInvoke<T>(cmd)
      : await _rawInvoke<T>(cmd, args);
  } catch (err) {
    const shape = JSON.stringify(shapeArgs(args));
    const raw = String(err);
    const truncated = raw.length > ERROR_TEXT_CAP
      ? raw.slice(0, ERROR_TEXT_CAP) + '…'
      : raw;
    logFrontend('error', `[IPC:${cmd}] args=${shape} — ${truncated}`);
    throw err;
  }
}

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
export const createAgentNode = (meshId: number, name: string, path: string, branch: string, provider?: string, useWorktree?: boolean) =>
  _invoke<AgentNode>('create_agent_node', { meshId, name, path, branch, provider, useWorktree });

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

/** Set (or clear, with null) a mesh's accent colour hex. */
export const updateMeshColor = (meshId: number, color: string | null) =>
  _invoke('update_mesh_color', { meshId, color });

export const createTestMesh = (name: string) =>
  _invoke<Mesh>('create_test_mesh', { name });

export const listMeshes = () =>
  _invoke<Mesh[]>('list_meshes');

export const deleteMesh = (meshId: number) =>
  _invoke('delete_mesh', { meshId });

export const updateMeshLayout = (meshId: number, layout: 'grid' | 'single') =>
  _invoke('update_mesh_layout', { meshId, layout });

/** Persist new sidebar positions for a set of meshes: `[meshId, position]` pairs. */
export const updateMeshPositions = (updates: [number, number][]) =>
  _invoke('update_mesh_positions', { updates });

export const updateMeshName = (meshId: number, name: string) =>
  _invoke('update_mesh_name', { meshId, name });

// Provider-read memoisation lives in `providerCache.ts` so the global Vitest
// fixture can reset it even when a test fully mocks this public IPC module.

export const getDefaultProvider = (meshId: number): Promise<string> => {
  let p = getDefaultProviderPromise(meshId);
  if (!p) {
    p = _invoke<string>('get_default_provider', { meshId });
    p.catch(() => { deleteDefaultProviderPromise(meshId); });
    setDefaultProviderPromise(meshId, p);
  }
  return p;
};

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
) => _invoke<void>('update_mesh_column', { meshId, column, value });

export const updateMeshUseWorktree = (meshId: number, useWorktree: boolean) =>
  _invoke<void>('update_mesh_use_worktree', { meshId, useWorktree });

/** Toggle whether this mesh's agent nodes run inside an OS process sandbox
 *  (Windows AppContainer #498 / macOS Seatbelt #497). Dedicated command (typed
 *  bool + zero-rows-is-an-error contract), like `updateMeshUseWorktree`. */
export const updateMeshSandbox = (meshId: number, sandbox: boolean) =>
  _invoke<void>('update_mesh_sandbox', { meshId, sandbox });

/** Every live Autopilot run's `(node_id, state)` — the header pill's data.
 *  Fetched alongside the node list in `fetchAgentNodes`. */
export const listAutopilotRuns = () =>
  _invoke<AutopilotRunStateRow[]>('list_autopilot_runs');

/** Persist a mesh's Autopilot Policy in one write (issue #481, PRD #480).
 *  Dedicated typed command like `updateMeshSandbox` — the backend
 *  range-checks the concurrency limit (1..=8) and collapses blank
 *  label/provider/action strings to NULL (poller defaults apply). */
export const updateMeshAutopilot = (
  meshId: number,
  enabled: boolean,
  triggerLabel: string | null,
  concurrencyLimit: number,
  provider: string | null,
  actionOnSuccess: string | null
) =>
  _invoke<void>('update_mesh_autopilot', {
    meshId,
    enabled,
    triggerLabel,
    concurrencyLimit,
    provider,
    actionOnSuccess,
  });

/** Persist a mesh's Looping Autopilot config in one write (wayfinder #990,
 *  ticket #991 backend / #994 UI). Dedicated typed command like
 *  `updateMeshAutopilot` — the backend trims blank prompts to NULL and
 *  range-checks `maxIterations >= 1 when Some`,
 *  `intervalSeconds >= 0`, `consecutiveFailures >= 0`; the six
 *  `loop_*` + `autopilot_mode` columns land atomically so the loop
 *  scheduler (#992) reads them as one config. The mode toggle
 *  (Issue-Driven vs Looping) and every numeric/prompt control funnel
 *  through this command — there is no per-field write path for loops. */
export const updateMeshLoopConfig = (
  meshId: number,
  mode: AutopilotMode,
  initialPrompt: string | null,
  suffixPrompt: string | null,
  maxIterations: number | null,
  intervalSeconds: number,
  consecutiveFailures: number
) =>
  _invoke<void>('update_mesh_loop_config', {
    meshId,
    mode,
    initialPrompt,
    suffixPrompt,
    maxIterations,
    intervalSeconds,
    consecutiveFailures,
  });

/** Looping Autopilot Start/Stop — flips ONLY `autopilot_enabled` (ticket #994).
 *  The poller (`services::autopilot`) spawns iterations for any mesh in Looping
 *  mode where this flag is true AND a non-empty `loop_initial_prompt` is set;
 *  the change takes effect on the next poll pass (≤ 2 min), no restart. Narrow
 *  dedicated command so Start/Stop can't clobber the issue-driven policy columns
 *  (`updateMeshAutopilot` owns those). */
export const setMeshAutopilotEnabled = (meshId: number, enabled: boolean) =>
  _invoke<void>('set_mesh_autopilot_enabled', { meshId, enabled });

/** Looping Autopilot runtime status for the Autopilot Probe tab's badge
 *  (ticket #994) — the `autopilot_enabled` flag + the loop-iteration ledger
 *  projected into Active N / Idle / Stopped. Thin wrapper over `get_loop_status`;
 *  the DB is the source of truth (there is no separate scheduler state). */
export const getLoopStatus = (meshId: number) =>
  _invoke<LoopStatusDto>('get_loop_status', { meshId });

/** Autopilot compatibility verdict for a Mesh (issue #1152). Pure
 *  read-side command — walks the resolved Autopilot Spawn Option
 *  (explicit Autopilot selection → mesh default → app default →
 *  "claude" fallback) and the harness capability contract, returning a
 *  structured verdict. The Probe UI gates enable/start controls on this;
 *  the backend `update_mesh_autopilot` and `set_mesh_autopilot_enabled`
 *  commands enforce the same verdict on the write side. Refetch on every
 *  relevant change (default provider, explicit Autopilot selection,
 *  worktree toggle, harness availability). */
export const getAutopilotCompatibility = (meshId: number) =>
  _invoke<AutopilotCompatibility>('get_autopilot_compatibility', { meshId });

/** Per-mesh target for the pre-spawn Worktree Pool
 *  (`services::warm_pool`, issue #611). `0` disables the pool for the
 *  mesh; `1..=5` is the target the worker fills to. Dedicated command
 *  so the typed integer + `0..=5` invariant are enforced at the IPC
 *  boundary (the catch-all `update_mesh_column` is intentionally
 *  unvalidated). The Worktrees Probe's ConfigurationCard toggle
 *  derives `enabled = poolSize > 0`; the size input clamps to 1..5. */
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

export const getMeshScratchpad = (meshId: number): Promise<string> => {
  let p = scratchpadByMesh.get(meshId);
  if (!p) {
    p = _invoke<string>('get_mesh_scratchpad', { meshId });
    p.catch(() => { scratchpadByMesh.delete(meshId); });
    scratchpadByMesh.set(meshId, p);
  }
  return p;
};

export const setMeshScratchpad = (meshId: number, content: string): Promise<void> => {
  // Optimistic write: seed the cache with the new value so the next
  // `get` resolves to what the editor just typed, even if the
  // underlying IPC is in flight. The rejected-promise evicts the
  // slot so a failed write doesn't poison future reads.
  const p = _invoke<void>('set_mesh_scratchpad', { meshId, content }).then(() => undefined);
  scratchpadByMesh.set(meshId, Promise.resolve(content));
  p.catch(() => { scratchpadByMesh.delete(meshId); });
  return p;
};

import type { DetectedProject } from './projectPresets';

export const detectMeshProject = (meshPath: string) =>
  _invoke<DetectedProject>('detect_mesh_project', { meshPath });

// Agent
export const spawnAgent = (
  sessionId: number,
  provider: string,
  resume?: string | null,
  rows?: number,
  cols?: number,
) => _invoke('spawn_agent', { sessionId, provider, resume, rows, cols });

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

/** Raw write to the agent's PTY (no submit/newline handling — cf. `sendToAgent`). */
export const writeToAgent = (sessionId: number, data: string) =>
  _invoke('write_to_agent', { sessionId, data });

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

export const mergePr = (prUrl: string) =>
  _invoke<string>('merge_pr', { prUrl });

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
  _invoke<GitHubIssue[]>('get_repo_issues', { meshId });

/** Add or remove a label on a mesh's GitHub issue (issue #979).
 *  Errors from a missing-repo-label 422 surface as a typed
 *  `GitHubError::LabelNotFound` message — see `commands::pr::set_issue_label`
 *  for the full contract. */
export const setIssueLabel = (
  meshId: number,
  issueNumber: number,
  label: string,
  action: 'add' | 'remove',
): Promise<void> =>
  _invoke<void>('set_issue_label', { meshId, issueNumber, label, action });

// GitHub Pull Requests — `GitHubPullRequest` / `PrMergeability` are generated
// from the Rust structs (src-tauri/src/commands/pr.rs) into
// src/types/generated/; see top import. Re-exported here so the PR probe tab
// can `import { GitHubPullRequest } from '../lib/tauri'` alongside the issue
// types. Issue #359.
export type { GitHubPullRequest, PrMergeability, PrMergeabilityEntry, PrFileEntry };

/** List PRs for a mesh's repo, filtered by `state` (`'open'` or `'closed'`). */
export const getRepoPulls = (meshId: number, state: 'open' | 'closed') =>
  _invoke<GitHubPullRequest[]>('get_repo_pulls', { meshId, state });

/// Per-PR mergeability enrichment — the `/pulls` list endpoint omits it, so
/// the panel fetches this once per open PR. `mergeable` is `null` while
/// GitHub is still computing the merge. **Deprecated on desktop** — use
/// [`getPrsMergeability`] for the batched call (issue #418); the per-PR
/// shape survives for the mobile HTTP route at
/// `GET /api/meshes/{id}/pulls/{n}/mergeability`.
export const getPrMergeability = (meshId: number, prNumber: number) =>
  _invoke<PrMergeability>('get_pr_mergeability', { meshId, prNumber });

/// Batched PR mergeability (issue #418). One IPC round-trip resolves the
/// GitHub token once and loops over the requested PR numbers — replacing
/// the per-PR fan-out the panel used to fire. Each entry carries the PR
/// `number` so the frontend can key results back onto the listed PRs.
/// `mergeable: null` is either "GitHub still computing" or "this PR's
/// individual probe failed" — both render as "Checking…" in the panel
/// (see [`get_prs_mergeability`] in `commands/pr.rs` for the rationale).
export const getPrsMergeability = (meshId: number, prNumbers: number[]) =>
  _invoke<PrMergeabilityEntry[]>('get_prs_mergeability', { meshId, prNumbers });

/// List the files changed in a single PR (issue #421). Backed by GitHub's
/// `/pulls/{n}/files` endpoint; one call returns the whole PR with each
/// file's unified-diff `patch`. The Center Diff Overlay parses the patch
/// line-by-line to render +/−/context rows. Distinct from `getRepoPulls` /
/// `getPrMergeability` because the panel needs the diff payload, not just
/// the metadata.
export const getPrFiles = (meshId: number, prNumber: number) =>
  _invoke<PrFileEntry[]>('get_pr_files', { meshId, prNumber });

export const spawnIssueAgent = (meshId: number, issueNumber: number, issueTitle: string, provider?: string) =>
  _invoke<AgentNode>('spawn_issue_agent', { meshId, issueNumber, issueTitle, provider });

/// Fast acceptance of an issue spawn. The backend commits the `pending` row,
/// starts the slow intent-driven launch in the background, and later emits
/// `node-spawn-completed` or `node-spawn-failed`. The returned draft keeps the
/// existing wire shape for compatibility; callers no longer need to hand the
/// transient prefill to a second IPC command.
export type { IssueNodeDraft };

export const createIssueNode = (meshId: number, issueNumber: number, issueTitle: string, provider?: string) =>
  _invoke<IssueNodeDraft>('create_issue_node', { meshId, issueNumber, issueTitle, provider });

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
  });

// AI context portability
export type { AiContextStatus };

export const detectAiContext = (meshPath: string) =>
  _invoke<AiContextStatus>('detect_ai_context', { meshPath });

export const createAiContextPortabilityPr = (meshId: number) =>
  _invoke<string>('create_ai_context_portability_pr', { meshId });

export const listProviders = (): Promise<ProviderInfo[]> => {
  let promise = getProviderListPromise();
  if (!promise) {
    promise = _invoke<ProviderInfo[]>('list_providers');
    promise.catch(() => { setProviderListPromise(null); });
    setProviderListPromise(promise);
  }
  return promise;
};

/** Provider UI metadata for the agent picker — generated from the Rust struct
 *  in `src-tauri/src/agent/provider/mod.rs` (issue #404). */
export type { ProviderInfo };

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

// ── Agent PTY transport ────────────────────────────────────────────────────
//
// `writeToAgent` is declared above next to the other agent IPCs; `resizeAgent`
// here completes the PTY-side surface used by `TerminalRegistry`. It rejects
// with the string `'Agent not running'` while the PTY isn't up yet — callers
// match on that exact value to ignore the expected race (see
// `TerminalRegistry.syncPtySize`).
export const resizeAgent = (sessionId: number, rows: number, cols: number) =>
  _invoke('resize_agent', { sessionId, rows, cols });

/** Reply to a remote-pane snapshot request from the HTTP server. The pair
 *  (`request_id`, `data`) is matched against an in-flight promise on the
 *  backend; the call returns immediately and has no result. */
export const submitTerminalSnapshot = (requestId: string, data: string) =>
  _invoke<void>('submit_terminal_snapshot', { requestId, data });

// ── Build/Run side-panel PTY ───────────────────────────────────────────────
//
// Separate PTY surface from the agent terminal — keeps a long-running build
// or `npm run dev` independent of the agent's PTY lifecycle. `build_run`
// returns once the child has been spawned; output flows via the
// `build-run-output-<nodeId>` event.
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

export const getAppPreferences = () =>
  _invoke<AppPreferences>('get_app_preferences');

/** Pass `null` (or an empty string, which the backend filters out) to clear
 *  the override and fall back to the hardcoded `anthropic` default. */
export const setAppDefaultProvider = (provider: string | null) =>
  _invoke('set_app_default_provider', { provider });

/** Issue #824: pick the backend that summarises PTY output into a slug.
 *  Pass `null` (or empty) to **disable** auto-naming — nodes keep their
 *  random `adjective-adjective-noun` slugs until the user picks a value
 *  in Settings → Auto-naming. Distinct from `default_provider`: a
 *  rename runs frequently on trivial content, so the user opts in
 *  explicitly rather than inheriting whatever expensive tier the
 *  spawned node happens to be on. */
export const setAppNamingProvider = (provider: string | null) =>
  _invoke('set_app_naming_provider', { provider });

/** App-wide autopilot pool cap (`null` = uncapped, `0` = pause new spawns).
 *  Semantics documented on `AppPreferences::autopilot_pool_size`. */
export const setAppAutopilotPoolSize = (size: number | null) =>
  _invoke('set_app_autopilot_pool_size', { size });

// ── Application-level Agent Harness defaults (issue #1150 / #1148) ──────────
//
// Sparse map keyed by stable harness profile id (`"claude"`, `"codex"`,
// `"agy"`, plus user-defined custom profiles). The backend validates each
// write against the harness's capability descriptor — unknown harness ids,
// effort values outside `effort_control.allowed`, and whitespace-only inputs
// are rejected at the boundary. An empty post-validation value removes the
// sparse map entry rather than storing `{model: null, effort: null}`.

/** Upsert one harness's application default. Errors propagate verbatim from
 *  the backend (the parent surfaces them through the existing settings-error
 *  feedback loop). */
export const setHarnessDefault = (
  profileId: string,
  value: HarnessConfigValue,
) => _invoke('set_harness_default', { profileId, value });

/** Remove one harness's application default. Idempotent — clearing an already-
 *  cleared harness is a no-op (so the UI's "Reset" affordance never errors). */
export const clearHarnessDefault = (profileId: string) =>
  _invoke('clear_harness_default', { profileId });

// ── Per-Mesh harness overrides (issue #1151 / slice 2 of #1148) ─────────────
//
// Sparse map keyed by stable harness profile id, scoped to a single Mesh.
// The cascade order at the spawn seam is now:
//   explicit > mesh_override > mesh (legacy) > application > native
// (the legacy `meshes.model` / `meshes.effort` columns remain physically
// present for positional compatibility but are no longer read as active
// configuration after the v33 migration). The Mesh Properties tab uses
// these three commands for the per-harness override list — Add / Edit /
// Reset-all affordances. The harness-id and effort-vocabulary rules are
// shared with `setHarnessDefault` — the backend validates against the
// harness's capability descriptor at the write boundary.

/** Upsert one harness's Mesh override. An empty post-validation value
 *  removes the sparse map entry rather than storing `{model: null,
 *  effort: null}`. */
export const upsertMeshHarnessOverride = (
  meshId: number,
  harnessId: string,
  value: HarnessConfigValue,
) => _invoke('upsert_mesh_harness_override', { meshId, harnessId, value });

/** Remove one harness's Mesh override. Idempotent — clearing an already-
 *  cleared harness is a no-op so the UI's "Reset" affordance never errors. */
export const removeMeshHarnessOverride = (meshId: number, harnessId: string) =>
  _invoke('remove_mesh_harness_override', { meshId, harnessId });

/** Reset every entry in the mesh's `harness_overrides` map — the secondary
 *  "Reset all" bulk action on the Mesh Properties tab. Idempotent on a
 *  mesh that has no overrides. Does NOT touch the application-level
 *  defaults map — the mesh simply inherits every application default. */
export const clearMeshHarnessOverrides = (meshId: number) =>
  _invoke('clear_mesh_harness_overrides', { meshId });

export const setMinimaxApiKey = (key: string | null) =>
  _invoke('set_minimax_api_key', { key });

/** Persist the spawn-menu harness order (issue #573). `order` is the list of
 *  harness-row ids top-to-bottom; Terminal is filtered out backend-side and is
 *  always pinned last. Busts the cached provider list AFTER the write resolves
 *  (same reasoning as `upsertProviderAccount`) so the next `listProviders` reads
 *  the reordered menu. */
export const setHarnessOrder = async (order: string[]) => {
  try {
    return await _invoke('set_harness_order', { order });
  } finally {
    setProviderListPromise(null);
  }
};

/** Persist the **Proxied Provider** child order under one harness (issue #577).
 *  `providerIds` is the top-to-bottom list of `provider_id`s the user arranged
 *  via the drag list on the harness-config page. Cross-harness drag is
 *  disallowed at the UI layer (each `HarnessCard` is its own dnd-kit context),
 *  so the `harnessId` + `providerIds` pair is the entire scope. Backend-side
 *  unknown-account ids are silently dropped — the order seam would never
 *  render them anyway. Busts the cached provider list AFTER the write resolves
 *  so every spawn surface (sidebar, probe tabs, archived-resume, mobile)
 *  re-reads the reordered menu on the next read. */
export const setProxiedProviderOrder = async (
  harnessId: string,
  providerIds: string[],
) => {
  try {
    return await _invoke('set_proxied_provider_order', {
      harnessId,
      providerIds,
    });
  } finally {
    setProviderListPromise(null);
  }
};

// ── Model provider accounts (issue #537 / ADR-0025) ─────────────────────────
//
// Generated from `crate::preferences::{ProviderAccount, BillingMode}`.
// Credentials + billing only; endpoint URL and model tiers live on pairings.
export type { ProviderAccount, BillingMode, ModelTiers };

/** Self-auth built-ins + keyed first-class / generics the user has added. */
export const getProviderAccounts = () =>
  _invoke<ProviderAccount[]>('get_provider_accounts');

/** Keyed first-class catalog (MiniMax / Kimi / OpenRouter) for Add provider. */
export const getKeyedFirstClassCatalog = () =>
  _invoke<ProviderAccount[]>('get_keyed_first_class_catalog');

export const upsertProviderAccount = async (account: ProviderAccount) => {
  try {
    return await _invoke('upsert_provider_account', { account });
  } finally {
    // ADR-0025: spawn visibility comes from stored pairings + the catalog,
    // not from account-side pairing registration. Still bust the cache so
    // the menu drops / shows the row on the next listProviders.
    setProviderListPromise(null);
  }
};

export const removeProviderAccount = async (id: string) => {
  try {
    return await _invoke('remove_provider_account', { id });
  } finally {
    setProviderListPromise(null);
  }
};

// ── Proxied Provider pairings (ADR-0016 §4 / ADR-0025, issue #576) ──────────
//
// Stored pairings only. API key is global on the provider; base URL + model
// tiers are per harness×provider pairing (edited on the Harnesses page).
export type { ApiSurface, EnvType, ProviderPairing, PairingVerification };

/** Stored pairings for proxiable accounts (spawn menu + harness config). */
export const getProviderPairings = () =>
  _invoke<ProviderPairing[]>('get_provider_pairings');

export const getPairingVerifications = (envType: EnvType = 'windows') =>
  _invoke<PairingVerification[]>('get_pairing_verifications', { envType });

export const verifyProviderPairing = (
  harnessId: string,
  providerId: string,
  envType: EnvType = 'windows',
) => _invoke<PairingVerification>('verify_provider_pairing', { harnessId, providerId, envType });

/** First-class attach defaults for a (harness, provider) pair, if compatible. */
export const getPairingDefaults = (harnessId: string, providerId: string) =>
  _invoke<ProviderPairing | null>('get_pairing_defaults', { harnessId, providerId });

/** Providers offered by "Add proxied provider" for `harnessId`, surface-matched. */
export const compatibleProvidersForHarness = (harnessId: string) =>
  _invoke<ProviderAccount[]>('compatible_providers_for_harness', { harnessId });

/** Attach a provider; client supplies base URL (+ Anthropic model tiers). */
export const attachProxiedProvider = async (
  harnessId: string,
  providerId: string,
  apiKey: string | null,
  baseUrl: string | null,
  modelTiers: ModelTiers | null,
) => {
  try {
    return await _invoke('attach_proxied_provider', {
      harnessId,
      providerId,
      apiKey,
      baseUrl,
      modelTiers,
    });
  } finally {
    setProviderListPromise(null);
  }
};

/** Edit base URL / model tiers on a stored pairing. */
export const updateProviderPairing = async (
  harnessId: string,
  providerId: string,
  baseUrl: string | null,
  modelTiers: ModelTiers | null,
) => {
  try {
    return await _invoke('update_provider_pairing', {
      harnessId,
      providerId,
      baseUrl,
      modelTiers,
    });
  } finally {
    setProviderListPromise(null);
  }
};

/** Detach a stored Proxied Provider pairing. */
export const removeProviderPairing = async (
  harnessId: string,
  providerId: string,
) => {
  try {
    return await _invoke('remove_provider_pairing', { harnessId, providerId });
  } finally {
    setProviderListPromise(null);
  }
};

// ── Provider usage (Accounts & Usage panel) ────────────────────────────────
//
// Generated from `crate::services::usage::{ProviderUsage, UsageWindow}`
// (issue #404). Rust uses `#[serde(rename = "...")]` + matching
// `#[ts(rename = "...")]` on some fields, so the camelCase / snake_case
// mix is exact — `usedPercent` / `resetsAt` / `loggedIn` are camelCase on
// the wire, the rest are snake_case.
export type { UsageWindow, ProviderUsage, BillingBalance, ProviderMeters };

/** The detection-gated Providers page rows: one entry per provider relevant to
 *  this host, each carrying its Usage Meters (or a "usage not tracked" marker).
 *  Reuses the `ProviderUsage` wire shape (issue #574). */
export const getProviderMeters = (forceRefresh: boolean) =>
  _invoke<ProviderMeters[]>('get_provider_meters', { forceRefresh });

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

// Cert status (issue #635). The QR modal surfaces the server's current root
// fingerprint so a user whose installed root CA is stale can see the mismatch
// and re-install. Only the desktop reads `cert_path` (the HTTP route omits it
// to avoid leaking the Windows username across the LAN).
import type { CertChainStatus } from '../types/generated/CertChainStatus';
export type { CertChainStatus };

export const getCertChainStatus = () =>
  _invoke<CertChainStatus>('get_cert_chain_status');

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

/** Test-only: clear the module-level provider caches between cases. Exported
 *  with a leading-underscore name so accidental production use is loud. */
export function __resetProviderCachesForTests(): void {
  resetProviderCachesForTests();
}

// ── Autopilot Circuits (spec #1205 / walking skeleton #1206) ─────────────
import type { AutopilotCircuit } from '../types/generated/AutopilotCircuit';
import type { CircuitRunDetail } from '../types/generated/CircuitRunDetail';
import type { CircuitWithRuns } from '../types/generated/CircuitWithRuns';
// Milestone 4 (#1209): the canvas editor consumes the blueprint AST, so
// the graph wire types ride the same sanctioned surface.
export type {
  AutopilotCircuit,
  CircuitRunDetail,
  CircuitWithRuns,
};
export type { CircuitGraph } from '../types/generated/CircuitGraph';
export type { CircuitNode } from '../types/generated/CircuitNode';
export type { CircuitNodeKind } from '../types/generated/CircuitNodeKind';
export type { CircuitEdge } from '../types/generated/CircuitEdge';
export type { EdgeCondition } from '../types/generated/EdgeCondition';
export type { StepOutcome } from '../types/generated/StepOutcome';
export type { GithubActionKind } from '../types/generated/GithubActionKind';
export type { SessionStatusKind } from '../types/generated/SessionStatusKind';

export const listCircuits = (meshId: number) =>
  _invoke<AutopilotCircuit[]>('list_circuits', { meshId });

/** One circuit row — the canvas editor overlay's load unit (#1209). */
export const getCircuit = (circuitId: number) =>
  _invoke<AutopilotCircuit>('get_circuit', { circuitId });

/** Batched single-IPC load for the Probe tab: every circuit on the mesh
 *  with up to `limit` newest runs each (steps included). */
export const listCircuitsWithRuns = (meshId: number, limit?: number) =>
  _invoke<CircuitWithRuns[]>('list_circuits_with_runs', { meshId, limit });

/** Creates a circuit with the canonical server-side blueprint:
 *  <trigger> → SpawnAgentNode (fresh) → InjectPty(prompt) → Notify.
 *  Trigger vocabulary (issue #1208): manual (default), interval,
 *  github_issue_label, github_pr_label. */
export type { CircuitTriggerKind } from '../types/generated/CircuitTriggerKind';
import type { CircuitTriggerKind } from '../types/generated/CircuitTriggerKind';

export const createCircuit = (
  meshId: number,
  name: string,
  description: string,
  concurrencyLimit: number,
  initialPrompt: string,
  triggerKind: CircuitTriggerKind = 'manual',
  triggerLabel?: string,
  intervalSeconds?: number
) =>
  _invoke<AutopilotCircuit>('create_circuit', {
    meshId,
    name,
    description,
    concurrencyLimit,
    initialPrompt,
    triggerKind,
    triggerLabel: triggerLabel ?? null,
    intervalSeconds: intervalSeconds ?? null,
  });

export const setCircuitEnabled = (circuitId: number, enabled: boolean) =>
  _invoke<void>('set_circuit_enabled', { circuitId, enabled });

/** Canvas editor save seam (issue #1209): replace the whole blueprint
 *  AST. The backend validates the JSON parses before persisting. */
export const updateCircuitGraph = (circuitId: number, graphJson: string) =>
  _invoke<void>('update_circuit_graph', { circuitId, graphJson });

export const deleteCircuit = (circuitId: number) =>
  _invoke<void>('delete_circuit', { circuitId });

export const triggerCircuitNow = (circuitId: number) =>
  _invoke<number>('trigger_circuit_now', { circuitId });

/** Graceful pause: the graph stops advancing; current steps finish (#1207). */
export const pauseCircuitRun = (runId: number) =>
  _invoke<void>('pause_circuit_run', { runId });

/** Resume a paused run where it stopped (#1207). */
export const resumeCircuitRun = (runId: number) =>
  _invoke<void>('resume_circuit_run', { runId });

/** Approve a CollaboratorCheck gate parked in `blocked` (#1207). */
export const approveCircuitStep = (runId: number, nodeId: string) =>
  _invoke<void>('approve_circuit_step', { runId, nodeId });

export const listCircuitRuns = (circuitId: number, limit?: number) =>
  _invoke<CircuitRunDetail[]>('list_circuit_runs', { circuitId, limit });