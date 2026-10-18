import { _invoke } from './_invoke';
import { emit } from '@tauri-apps/api/event';
import { deleteDefaultProviderPromise } from '../providerCache';
import { PROVIDER_LIST_CHANGED_EVENT } from './provider';
import type { Mesh } from '../../types/generated/Mesh';
import type { AiContextStatus } from '../../types/generated/AiContextStatus';
import type { MeshRow } from '../../types/generated/MeshRow';
import type { PickedFolder } from '../../types/generated/PickedFolder';
import type { DetectedProject } from '../projectPresets';

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

export type { WorktreeDirectoryConfig } from '../../types/generated/WorktreeDirectoryConfig';

/** Effective worktree directory config for one Mesh (issue #1519) —
 *  override + app default + resolved effective container for the
 *  Project Settings → Worktrees inherited-value display. */
export const getWorktreeDirectoryConfig = (meshId: number) =>
  _invoke<import('../../types/generated/WorktreeDirectoryConfig').WorktreeDirectoryConfig>(
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

export const detectMeshProject = (meshPath: string) =>
  _invoke<DetectedProject>('detect_mesh_project', { meshPath });

// AI context portability
export type { AiContextStatus };

export const detectAiContext = (meshPath: string) =>
  _invoke<AiContextStatus>('detect_ai_context', { meshPath });

export const createAiContextPortabilityPr = (meshId: number) =>
  _invoke<string>('create_ai_context_portability_pr', { meshId });
