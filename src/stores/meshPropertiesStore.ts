import { create } from 'zustand';
import * as api from '../lib/tauri';
import type { DetectedProject, ProjectPreset } from '../lib/projectPresets';

/// The wire shape of the panel's loaded config. Re-export from `tauri.ts`
/// so consumers see one canonical `MeshConfig` (the Rust struct in
/// `src-tauri/src/models/mod.rs` doesn't yet derive `TS` — see ADR-0009
/// follow-up).
export type { MeshConfig } from '../lib/tauri';
import type { MeshConfig } from '../lib/tauri';

/// The set of fields the MeshPropertiesPanel can auto-save. The union is the
/// store's switch table — adding a field means one new entry below + one new
/// case in `save()`, not a new closure in the render component (issue #283).
///
/// `useWorktree` and `baseRef` route through dedicated backend commands
/// because they have side-effects beyond a field write (DB state, settings.json).
export type MeshConfigField =
  | 'model'
  | 'effort'
  | 'defaultProvider'
  | 'worktreeMode'
  | 'buildCommand'
  | 'runCommand'
  | 'useWorktree'
  | 'baseRef';

interface MeshPropertiesState {
  meshId: number | null;
  config: MeshConfig | null;
  detected: DetectedProject | null;
  loading: boolean;
  error: string | null;

  /// Load config + best-effort project detection for one mesh. Sets `loading`
  /// while in flight; on completion both `config` and (optionally) `detected`
  /// are populated. A detection failure is swallowed to `detected: null` —
  /// the panel renders fine without it, and a missing `package.json` is not
  /// an error the user can act on.
  load: (meshId: number, meshPath: string) => Promise<void>;

  /// Persist a single panel field. The dispatch table routes to the right
  /// backend command (`update_mesh_field` for plain `mesh.toml` writes,
  /// `update_mesh_use_worktree` / `update_worktree_base_ref` for fields
  /// with DB-side / settings.json side-effects). Throws on backend error
  /// so the caller can choose to suppress side-effects (e.g. `git.refresh()`).
  save: (field: MeshConfigField, value: string | boolean) => Promise<void>;

  /// Apply a project preset's build/run commands in parallel — concurrently
  /// fires the two `update_mesh_field` writes so the panel sees one round-trip
  /// of latency, not two.
  applyPreset: (preset: ProjectPreset) => Promise<void>;

  /// Clear panel state on close so a re-open starts from `loading=true`
  /// rather than briefly flashing the previous mesh's values.
  reset: () => void;
}

const INITIAL: Omit<MeshPropertiesState, 'load' | 'save' | 'applyPreset' | 'reset'> = {
  meshId: null,
  config: null,
  detected: null,
  loading: false,
  error: null,
};

export const useMeshPropertiesStore = create<MeshPropertiesState>((set, get) => ({
  ...INITIAL,

  load: async (meshId, meshPath) => {
    // Clear `config` AND `detected` on entry so a slow / failed load
    // never leaves the PREVIOUS mesh's values mirrored into the panel form
    // under the new mesh's header — that would silently let the user
    // edit field X on mesh A and have the save() write it against mesh B.
    set({ meshId, config: null, detected: null, loading: true, error: null });
    // Fire both IPCs in parallel — they're independent. Pre-fix the panel
    // open paid (config-latency + detection-latency); now it pays the max.
    // Detection is best-effort — `detect_mesh_project` failing (no
    // package.json, unreadable mesh dir) must not turn the whole load
    // into an error.
    const [configResult, detectedResult] = await Promise.allSettled([
      api.getMeshProperties(meshId),
      api.detectMeshProject(meshPath),
    ]);
    if (configResult.status === 'fulfilled') {
      set({ config: configResult.value });
    } else {
      set({ error: String(configResult.reason) });
    }
    set({
      detected: detectedResult.status === 'fulfilled' ? detectedResult.value : null,
      loading: false,
    });
  },

  save: async (field, value) => {
    const meshId = get().meshId;
    if (meshId == null) return; // defensive — UI should never call save before load
    try {
      switch (field) {
        case 'model':
          await api.updateMeshField(meshId, 'agent', 'model', String(value));
          break;
        case 'effort':
          // Empty effort means "Not set" — preserve the panel's existing rule
          // of never overwriting a real value with a blank by mistake.
          if (String(value) === '') return;
          await api.updateMeshField(meshId, 'agent', 'effort', String(value));
          break;
        case 'defaultProvider':
          await api.updateMeshField(meshId, 'agent', 'default_provider', String(value));
          break;
        case 'worktreeMode':
          await api.updateMeshField(meshId, 'agent', 'worktree_mode', String(value));
          break;
        case 'buildCommand':
          await api.updateMeshField(meshId, 'build', 'command', String(value));
          break;
        case 'runCommand':
          await api.updateMeshField(meshId, 'run', 'command', String(value));
          break;
        case 'useWorktree':
          await api.updateMeshUseWorktree(meshId, Boolean(value));
          break;
        case 'baseRef':
          await api.updateWorktreeBaseRef(meshId, String(value));
          break;
      }
    } catch (e) {
      set({ error: String(e) });
      throw e;
    }
  },

  applyPreset: async (preset) => {
    const meshId = get().meshId;
    if (meshId == null) return;
    // Concurrent — one round-trip of latency instead of two. Either rejection
    // bubbles via Promise.all (consumed by the caller for git-refresh gating).
    try {
      await Promise.all([
        api.updateMeshField(meshId, 'build', 'command', preset.build),
        api.updateMeshField(meshId, 'run', 'command', preset.run),
      ]);
    } catch (e) {
      set({ error: String(e) });
      throw e;
    }
  },

  reset: () => set({ ...INITIAL }),
}));
