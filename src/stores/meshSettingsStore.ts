import { formatError } from '../lib/errorUtils';
import { create } from 'zustand';
import * as api from '../lib/tauri';
import { getEffectiveWorktreeDir } from '../lib/paths';
import { useMeshStore } from './meshStore';
import type { AppPreferences, MeshRow, WorktreeDirectoryConfig } from '../lib/tauri';
import type { DetectedProject, ProjectPreset } from '../lib/projectPresets';

/**
 * `meshSettingsStore` — the per-Mesh **editing session** for the Probe panel's
 * Project Settings and Scratch Pad destinations (issue #2242).
 *
 * The store this file replaces (`meshPropertiesStore`, issue #283) was built
 * to own this role and then stopped being called: `ProjectSettingsTab` grew
 * its own inline copy — twenty `useState` calls covering the form and the
 * worktree strategy, seven near-identical `saveX` closures, an
 * `activeMeshIdRef` mirror and a hand-rolled `mountedRef` — while the store
 * sat behind a test that exercised code no user could reach. The drift was
 * visible in the shape itself: the store's field list still carried `model`
 * and `effort`, which the tab had dropped, and it knew nothing about the
 * worktree-strategy controls that #1460 moved into the same destination.
 *
 * What the store owns, and why it has to be the store:
 *
 * - **One session per Mesh.** `meshId` flips the moment a load starts, so every
 *   save can capture it at fire time and compare it on settle. That is the
 *   mesh-switch guard the tab used to hold in a ref: a slow write for the
 *   outgoing Mesh must not paint "Save failed" over the incoming one. The ref
 *   existed because the component had no owner for "which Mesh am I editing";
 *   this store is that owner, so the ref is gone rather than relocated.
 * - **Drafts.** The form mirrors user intent: a rejected write keeps the typed
 *   value so the message explains why it did not stick.
 * - **Request identity.** A load that resolves after the user has moved on is
 *   dropped rather than painting the previous Mesh's config under the new
 *   Mesh's header — the same authority rule `meshStore` applies to
 *   `list_meshes` (see `docs/development/probe-ui.md`).
 *
 * Because every async continuation writes here rather than into a component's
 * `useState`, a save that settles after the destination unmounts is harmless —
 * that is what the tab's hand-rolled `mountedRef` was guarding.
 *
 * Two error channels, both pre-existing: `status`/`statusError` drive the
 * destination's `<SaveIndicator>` for the text and select fields (issue #729),
 * and `strategyError` is section-local because a worktree-strategy failure has
 * to name the column that refused the write.
 */

// ── Wire ↔ form mapping for the worktree-strategy controls ──────────────────

/**
 * Wire-shape form values for the Starting point radio. The on-the-wire value
 * (passed to `update_worktree_base_ref`) is `'origin/main'` for fresh sessions,
 * `'HEAD'` for resuming.
 */
export type BaseRefForm = 'fresh' | 'head';

/** Wire-shape form values for the Worktree mode radio. */
export type WorktreeModeForm = 'branched' | 'detached';

/**
 * The default worktree mode for a fresh agent node. Local pin that must agree
 * with `DEFAULT_WORKTREE_MODE` in `src-tauri/src/agent/spawn.rs` (per the
 * cross-language default coupling pattern — see ADR follow-up). When the
 * `meshes.worktree_mode` column is missing or `null` on the wire, this is the
 * value the form falls back to.
 */
const DEFAULT_WORKTREE_MODE: WorktreeModeForm = 'branched';

// Mappers between the form's two-option enums and the open-ended string values
// stored in `meshes.base_ref` / `meshes.worktree_mode`. We intentionally
// collapse anything other than the canonical values to the form's default —
// the pre-#380 `MeshPropertiesPanel` used the same rule
// (`config.base_ref === 'HEAD' ? 'head' : 'fresh'` and
// `config.worktree_mode ?? DEFAULT_WORKTREE_MODE`).
export const wireToFormBaseRef = (wire: string | null): BaseRefForm =>
  wire === 'HEAD' ? 'head' : 'fresh';
export const wireToFormMode = (wire: string | null): WorktreeModeForm =>
  wire === 'detached' ? 'detached' : DEFAULT_WORKTREE_MODE;
export const formToWireBaseRef = (form: BaseRefForm): 'origin/main' | 'HEAD' =>
  form === 'head' ? 'HEAD' : 'origin/main';

// ── Save-status machine ────────────────────────────────────────────────────

/** 'Saving…' / 'Saved' / 'Save failed' indicator state (issue #729). */
export type SaveStatus = 'idle' | 'saving' | 'saved' | 'error';

/** How long `saved` sticks before returning to `idle` — long enough to read
 *  on a peripheral monitor, short enough not to linger. */
const SAVED_CLEAR_MS = 1500;

/** Scratch Pad writes are debounced so IPC chatter stays bounded while the
 *  user is mid-thought: one write per pause, not one per keystroke. */
const SCRATCHPAD_DEBOUNCE_MS = 500;

// ── Drafts ─────────────────────────────────────────────────────────────────

/** The identity / runtime / command fields, as the form holds them. */
export interface MeshSettingsForm {
  name: string;
  buildCommand: string;
  runCommand: string;
  rootBuildCommand: string;
  rootRunCommand: string;
  defaultProvider: string;
  sandbox: boolean;
}

/** The worktree-strategy controls, as the form holds them. */
export interface WorktreeStrategyDraft {
  useWorktree: boolean;
  baseRef: BaseRefForm;
  worktreeMode: WorktreeModeForm;
  /** `0` = pool off, `1..=5` = target. */
  preSpawnPoolSize: number;
  /** Per-Mesh override draft (`''` = inherit, issue #1519). */
  worktreeDirectory: string;
  /** Inherited effective container dir for display (backend-authoritative). */
  worktreeDirEffective: string;
  /** App-wide default, needed to re-resolve the effective dir client-side
   *  when the backend refresh fails. */
  worktreeDirAppDefault: string | null;
}

/** Every field the session can persist. Adding one is one case in `save()` —
 *  no new closure in the render component. */
export type SettingsField =
  | 'name'
  | 'buildCommand'
  | 'runCommand'
  | 'rootBuildCommand'
  | 'rootRunCommand'
  | 'defaultProvider'
  | 'sandbox'
  | 'useWorktree'
  | 'baseRef'
  | 'worktreeMode'
  | 'poolSize'
  | 'worktreeDirectory';

/**
 * Fields whose failure is reported in the section-local `strategyError`
 * instead of the destination's `SaveIndicator`. A worktree-strategy message has
 * to name the control that refused the write ("Failed to update base_ref"),
 * which one global banner cannot do.
 */
const STRATEGY_FIELDS: ReadonlySet<SettingsField> = new Set<SettingsField>([
  'useWorktree',
  'baseRef',
  'worktreeMode',
  'poolSize',
  'worktreeDirectory',
]);

/** Load request: the Mesh to edit, plus the two inputs the resolution chain
 *  needs. `fallbackName` is the sidebar's current name for the Mesh, used only
 *  when the stored config has none (see the load's fallback chain). */
export interface MeshSettingsLoad {
  meshId: number;
  meshPath: string;
  fallbackName: string | undefined;
}

interface MeshSettingsState {
  /** The Mesh this session is bound to. Flips when a load STARTS, so a save
   *  that settles after the switch can tell it no longer owns the form. */
  meshId: number | null;
  /** The Mesh whose config is currently applied. `null` while loading and
   *  after a failed load — the destinations gate their form on it. */
  loadedMeshId: number | null;
  meshPath: string | null;
  loading: boolean;
  loadError: string | null;
  /** Project-type detection for the mesh ROOT (drives the "Looks like an X
   *  project" hint and the preset dropdown's ✓ marker). Best-effort: a failure
   *  is `null`, never an error the user can act on. */
  detected: DetectedProject | null;
  form: MeshSettingsForm;
  strategy: WorktreeStrategyDraft;
  strategyError: string | null;
  status: SaveStatus;
  statusError: string | null;
  /** Live pre-spawn pool READY count (telemetry, not a draft). `null` = first
   *  fetch in flight, so the badge shows "…" rather than a misleading "0/N". */
  poolCount: number | null;
  /** Scratch Pad session state. Its Mesh id is tracked separately from `meshId`
   *  because the two destinations load independently (the Probe panel shows one
   *  at a time, and a Scratch Pad visit must not depend on a settings load). */
  scratchpad: {
    meshId: number | null;
    text: string;
    loadError: string | null;
    /** `false` while the read is in flight or after it failed — the textarea
     *  stays disabled so an edit can't be written to a row we never read. */
    loaded: boolean;
  };

  /** Load config + detection + directory resolution for one Mesh, applying the
   *  result only while this request is still the newest. */
  load: (request: MeshSettingsLoad) => Promise<void>;
  /** Edit a form draft without saving (text fields save on blur). */
  editForm: (patch: Partial<MeshSettingsForm>) => void;
  /** Persist one field: patch the draft, then write it to the Mesh. */
  save: (field: SettingsField, value: string | number | boolean) => Promise<void>;
  /** Fill Build/Run from a project preset — one round-trip of latency, not two. */
  applyPreset: (preset: ProjectPreset) => Promise<void>;
  /** Re-read the effective worktree directory (the app default moved
   *  elsewhere, issue #1519). */
  refreshWorktreeDirectory: () => Promise<void>;
  /** Re-read the pool readiness badge for the session's Mesh. */
  refreshPoolCount: (meshId: number | null) => void;
  /** Force the indicator back to `idle` (the banner's dismiss affordance). */
  resetSaveStatus: () => void;

  /** Load the Scratch Pad for a Mesh, flushing any unpersisted draft for the
   *  outgoing one first. */
  loadScratchpad: (meshId: number | null) => void;
  /** Record a Scratch Pad edit and schedule its debounced write. */
  editScratchpad: (text: string) => void;
  /** Write the pending Scratch Pad draft now, routed to ITS OWN Mesh. Called
   *  on mesh switch and on unmount so the last few keystrokes are not lost. */
  flushScratchpad: () => void;
}

const INITIAL_FORM: MeshSettingsForm = {
  name: '',
  buildCommand: '',
  runCommand: '',
  rootBuildCommand: '',
  rootRunCommand: '',
  defaultProvider: '',
  sandbox: false,
};

const INITIAL_STRATEGY: WorktreeStrategyDraft = {
  useWorktree: true,
  baseRef: 'fresh',
  worktreeMode: DEFAULT_WORKTREE_MODE,
  preSpawnPoolSize: 0,
  worktreeDirectory: '',
  worktreeDirEffective: '',
  worktreeDirAppDefault: null,
};

// Scratch Pad write bookkeeping lives outside the store: a pending timer is not
// state a subscriber should re-render for, and the pending write belongs to the
// Mesh it was typed for even once the session has moved on.
let scratchpadTimer: ReturnType<typeof setTimeout> | null = null;
let scratchpadPending: { meshId: number; text: string } | null = null;
/** Bumped per accepted edit and per load, so a save can tell whether it is
 *  still the newest write for the Mesh the user is looking at. */
let scratchpadRevision = 0;

/** Monotonic load identity (issue #2242). A mesh-id comparison is not enough:
 *  re-firing the load for the SAME Mesh (a retry, a path edit) must retire the
 *  previous read too, or the older response paints over the newer one. */
let loadToken = 0;

function clearScratchpadTimer() {
  if (scratchpadTimer !== null) {
    clearTimeout(scratchpadTimer);
    scratchpadTimer = null;
  }
}

export const useMeshSettingsStore = create<MeshSettingsState>((set, get) => {
  // ── Save-status machine (issue #729) ──────────────────────────────────────
  //
  //   idle    — nothing in flight, no last result worth showing
  //   saving  — IPC in flight; clears the prior error/saved hint
  //   saved   — last write succeeded; auto-clears after SAVED_CLEAR_MS
  //   error   — last write rejected; persists until the NEXT save starts, so
  //             the user actually sees the problem
  //
  // Single instance per session, NOT one per field: the destination shows one
  // indicator, and seven stacked indicators would compete for the same
  // attention as the form below them.
  let savedTimer: ReturnType<typeof setTimeout> | null = null;
  const cancelSavedTimer = () => {
    if (savedTimer !== null) {
      clearTimeout(savedTimer);
      savedTimer = null;
    }
  };
  const startSave = () => {
    cancelSavedTimer();
    set({ status: 'saving', statusError: null });
  };
  const saveSucceeded = () => {
    cancelSavedTimer();
    set({ status: 'saved', statusError: null });
    savedTimer = setTimeout(() => {
      savedTimer = null;
      // A `saved → saving` transition can race this timer; don't undo it.
      if (get().status === 'saved') set({ status: 'idle' });
    }, SAVED_CLEAR_MS);
  };
  const saveFailed = (e: unknown) => {
    cancelSavedTimer();
    set({ status: 'error', statusError: formatError(e) });
  };
  const resetSaveStatus = () => {
    cancelSavedTimer();
    set({ status: 'idle', statusError: null });
  };

  // ── Mesh-switch guard ────────────────────────────────────────────────────
  //
  // Both save channels capture the session's Mesh at the moment the IPC starts
  // and compare it on settle. Without that, a write for the outgoing Mesh
  // surfaces its result over the incoming Mesh's form — a "Saved" or a "Save
  // failed" that describes a project the user is no longer looking at.
  const ownsForm = (saveMeshId: number) => get().meshId === saveMeshId;

  const runFormSave = async (saveMeshId: number, op: () => Promise<void>) => {
    if (get().loadedMeshId !== saveMeshId || get().loading || get().loadError !== null) return;
    startSave();
    try {
      await op();
      if (!ownsForm(saveMeshId)) return;
      saveSucceeded();
    } catch (e) {
      // `console.error` preserves the audit trail in buildmesh.log for the
      // dev / bug-report path; the rendered banner shows the same message.
      console.error('Project setting save failed:', e);
      if (!ownsForm(saveMeshId)) return;
      saveFailed(e);
    }
  };

  /** Same guard, but the failure lands in the section-local `strategyError`.
   *  Resolves to whether the write landed, so a caller can gate a follow-up
   *  read on the same boundary. */
  const runStrategySave = async (
    saveMeshId: number,
    op: () => Promise<unknown>,
    failurePrefix: string,
  ): Promise<boolean> => {
    try {
      await op();
      return true;
    } catch (e) {
      console.error('Worktree strategy save failed:', e);
      if (!ownsForm(saveMeshId)) return false;
      set({ strategyError: `${failurePrefix}: ${formatError(e)}` });
      return false;
    }
  };

  /**
   * Applies a loaded directory config to the draft. Shared by the orchestrated
   * load and the `worktree-directory-changed` listener so the app-default /
   * effective pair can never be written by one path and not the other.
   */
  const applyWorktreeDirConfig = (
    config: Pick<MeshRow, 'worktree_directory'>,
    prefs: AppPreferences | null,
    dirConfig: WorktreeDirectoryConfig | null,
  ) => {
    const meshDir = config.worktree_directory?.trim() ?? '';
    const appDir = (prefs?.worktree_directory?.trim() ?? '') || null;
    set((state) => ({
      strategy: {
        ...state.strategy,
        worktreeDirectory: meshDir,
        worktreeDirAppDefault: appDir,
        worktreeDirEffective:
          dirConfig?.effective_directory ??
          getEffectiveWorktreeDir(state.meshPath ?? '', meshDir, appDir),
      },
    }));
  };

  /** The newest Scratch Pad write for the Mesh now on screen. */
  const ownsScratchpadWrite = (meshId: number, revision: number) =>
    get().scratchpad.meshId === meshId && scratchpadRevision === revision;

  return {
    meshId: null,
    loadedMeshId: null,
    meshPath: null,
    loading: false,
    loadError: null,
    detected: null,
    form: INITIAL_FORM,
    strategy: INITIAL_STRATEGY,
    strategyError: null,
    status: 'idle',
    statusError: null,
    poolCount: null,
    scratchpad: { meshId: null, text: '', loadError: null, loaded: false },

    load: async ({ meshId, meshPath, fallbackName }) => {
      const token = (loadToken += 1);
      // Clear the drafts on entry so a slow or failed load never leaves the
      // PREVIOUS Mesh's values in the form under the new Mesh's header — the
      // user would edit field X on Mesh A and have the save write it to Mesh B.
      set({
        meshId,
        meshPath,
        loadedMeshId: null,
        loading: true,
        loadError: null,
        detected: null,
        form: INITIAL_FORM,
        strategy: INITIAL_STRATEGY,
        // A "Failed to update base_ref" from the outgoing project describes
        // controls the user is no longer looking at, and it names a column
        // whose value is about to change underneath it.
        strategyError: null,
      });
      resetSaveStatus();
      const requestMeshId = meshId;
      try {
        const [config, detected, prefs, dirConfig] = await Promise.all([
          api.getMeshProperties(meshId),
          // Detection is best-effort: no `package.json`, or an unreadable mesh
          // root, is not an error the user can act on — the panel renders
          // fine without the hint.
          api.detectMeshProject(meshPath).catch(() => null),
          // The two secondary reads are individually best-effort: a failure to
          // resolve preferences or the directory config must degrade the
          // "Effective:" display, never blank the whole form.
          api.getAppPreferences().catch(() => null),
          api.getWorktreeDirectoryConfig(meshId).catch(() => null),
        ]);
        // Request identity: a load that resolved after a newer load started —
        // for a different Mesh, or the same one retried — describes a form the
        // user is no longer looking at.
        if (loadToken !== token) return;
        const folderName = meshPath.split(/[/\\]/).pop() ?? '';
        set({
          loadedMeshId: requestMeshId,
          loading: false,
          detected,
          form: {
            name: config.name || fallbackName || folderName,
            buildCommand: config.build_command ?? '',
            runCommand: config.run_command ?? '',
            rootBuildCommand: config.root_build_command ?? '',
            rootRunCommand: config.root_run_command ?? '',
            defaultProvider: config.default_provider ?? '',
            sandbox: config.sandbox,
          },
          strategy: {
            useWorktree: config.use_worktree,
            baseRef: wireToFormBaseRef(config.base_ref),
            worktreeMode: wireToFormMode(config.worktree_mode),
            preSpawnPoolSize: config.pre_spawn_pool_size,
            worktreeDirectory: '',
            worktreeDirEffective: '',
            // Resolved by `applyWorktreeDirConfig` in the same pass, so no
            // field is left showing the previous project's value.
            worktreeDirAppDefault: null,
          },
        });
        applyWorktreeDirConfig(config, prefs, dirConfig);
      } catch (e) {
        // A retired load must not report its failure either — the error would
        // replace a newer load's outcome under the same header.
        if (loadToken !== token) return;
        set({ loadError: formatError(e), loading: false });
      }
    },

    editForm: (patch) => set((state) => ({ form: { ...state.form, ...patch } })),

    save: async (field, value) => {
      const saveMeshId = get().meshId;
      if (saveMeshId === null) return;
      if (STRATEGY_FIELDS.has(field)) {
        set({ strategyError: null });
      }
      const text = String(value);
      switch (field) {
        case 'name': {
          // No-op write: skip the IPC and avoid a false "Saved" flicker for a
          // value the sidebar already holds.
          if (text === useMeshStore.getState().meshesById.get(saveMeshId)?.name) return;
          set((state) => ({ form: { ...state.form, name: text } }));
          // `updateMeshName` rethrows after recording its own error, so the
          // rejection reaches this channel (docs: "configuration saves reject,
          // they never resolve silently").
          await runFormSave(saveMeshId, () =>
            useMeshStore.getState().updateMeshName(saveMeshId, text),
          );
          return;
        }
        case 'buildCommand':
          set((state) => ({ form: { ...state.form, buildCommand: text } }));
          await runFormSave(saveMeshId, () =>
            api.updateMeshColumn(saveMeshId, 'build_command', text),
          );
          return;
        case 'runCommand':
          set((state) => ({ form: { ...state.form, runCommand: text } }));
          await runFormSave(saveMeshId, () =>
            api.updateMeshColumn(saveMeshId, 'run_command', text),
          );
          return;
        case 'rootBuildCommand':
          set((state) => ({ form: { ...state.form, rootBuildCommand: text } }));
          await runFormSave(saveMeshId, () =>
            api.updateMeshColumn(saveMeshId, 'root_build_command', text),
          );
          return;
        case 'rootRunCommand':
          set((state) => ({ form: { ...state.form, rootRunCommand: text } }));
          await runFormSave(saveMeshId, () =>
            api.updateMeshColumn(saveMeshId, 'root_run_command', text),
          );
          return;
        case 'defaultProvider':
          set((state) => ({ form: { ...state.form, defaultProvider: text } }));
          await runFormSave(saveMeshId, () =>
            api.updateMeshColumn(saveMeshId, 'default_provider', text),
          );
          return;
        case 'sandbox':
          set((state) => ({ form: { ...state.form, sandbox: Boolean(value) } }));
          await runFormSave(saveMeshId, () =>
            api.updateMeshSandbox(saveMeshId, Boolean(value)),
          );
          return;
        case 'useWorktree':
          set((state) => ({ strategy: { ...state.strategy, useWorktree: Boolean(value) } }));
          await runStrategySave(
            saveMeshId,
            () => api.updateMeshUseWorktree(saveMeshId, Boolean(value)),
            'Failed to update use_worktree',
          );
          return;
        case 'baseRef':
          set((state) => ({ strategy: { ...state.strategy, baseRef: value as BaseRefForm } }));
          await runStrategySave(
            saveMeshId,
            () => api.updateWorktreeBaseRef(saveMeshId, formToWireBaseRef(value as BaseRefForm)),
            'Failed to update base_ref',
          );
          return;
        case 'worktreeMode':
          set((state) => ({ strategy: { ...state.strategy, worktreeMode: value as WorktreeModeForm } }));
          await runStrategySave(
            saveMeshId,
            () => api.updateMeshColumn(saveMeshId, 'worktree_mode', String(value)),
            'Failed to update worktree_mode',
          );
          return;
        case 'poolSize': {
          // Clamp at the IPC boundary and in the handler, so a typed
          // out-of-range value never costs a round-trip to be rejected.
          const clamped = Math.max(0, Math.min(5, Math.trunc(Number(value)) || 0));
          set((state) => ({ strategy: { ...state.strategy, preSpawnPoolSize: clamped } }));
          await runStrategySave(
            saveMeshId,
            () => api.updateMeshPoolSize(saveMeshId, clamped),
            'Failed to update pool size',
          );
          return;
        }
        case 'worktreeDirectory': {
          const trimmed = text.trim();
          set((state) => ({ strategy: { ...state.strategy, worktreeDirectory: text } }));
          const saved = await runStrategySave(
            saveMeshId,
            () => api.updateMeshWorktreeDirectory(saveMeshId, trimmed === '' ? null : trimmed),
            'Failed to update worktree directory',
          );
          // Re-read the resolved path so the inherited display cannot drift
          // from the backend's precedence rule. Both reads re-check the
          // boundary: this is the one handler that writes three fields from a
          // second response.
          if (!saved || !ownsForm(saveMeshId)) return;
          try {
            const cfg = await api.getWorktreeDirectoryConfig(saveMeshId);
            if (!ownsForm(saveMeshId)) return;
            set((state) => ({
              strategy: {
                ...state.strategy,
                worktreeDirEffective: cfg.effective_directory,
                worktreeDirAppDefault: cfg.app_directory,
                worktreeDirectory: cfg.mesh_directory?.trim() ?? '',
              },
            }));
          } catch {
            // Keep the optimistic form — the save itself succeeded, and the
            // effective display refreshes on the next load.
            if (!ownsForm(saveMeshId)) return;
            set((state) => ({
              strategy: {
                ...state.strategy,
                worktreeDirEffective: getEffectiveWorktreeDir(
                  get().meshPath ?? '',
                  trimmed,
                  state.strategy.worktreeDirAppDefault,
                ),
              },
            }));
          }
          return;
        }
      }
    },

    applyPreset: async (preset) => {
      const saveMeshId = get().meshId;
      if (saveMeshId === null) return;
      set((state) => ({
        form: { ...state.form, buildCommand: preset.build, runCommand: preset.run },
      }));
      await runFormSave(saveMeshId, async () => {
        // Concurrent — one round-trip of latency instead of two.
        await Promise.all([
          api.updateMeshColumn(saveMeshId, 'build_command', preset.build),
          api.updateMeshColumn(saveMeshId, 'run_command', preset.run),
        ]);
      });
    },

    refreshWorktreeDirectory: async () => {
      const saveMeshId = get().meshId;
      if (saveMeshId === null) return;
      try {
        const [config, prefs, dirConfig] = await Promise.all([
          api.getMeshProperties(saveMeshId),
          api.getAppPreferences().catch(() => null),
          api.getWorktreeDirectoryConfig(saveMeshId).catch(() => null),
        ]);
        if (!ownsForm(saveMeshId)) return;
        applyWorktreeDirConfig(config, prefs, dirConfig);
      } catch {
        // Swallow — keep the existing form state.
      }
    },

    refreshPoolCount: (meshId) => {
      if (meshId === null) {
        set({ poolCount: null });
        return;
      }
      const requestMeshId = meshId;
      api.getWarmPoolCount(requestMeshId)
        .then((n) => {
          // Keep stale value on error rather than blanking the badge for a
          // transient SQLite hiccup.
          if (get().meshId === requestMeshId) set({ poolCount: n });
        })
        .catch(() => {
          /* keep stale value */
        });
    },

    resetSaveStatus,

    loadScratchpad: (meshId) => {
      // A draft typed for the outgoing Mesh belongs to THAT Mesh's row, so it
      // is flushed before the session moves on.
      get().flushScratchpad();
      // Fences the read as well as the writes: a Retry re-fires this for the
      // SAME mesh, and the older response must not win.
      const requestRevision = (scratchpadRevision += 1);
      resetSaveStatus();
      if (meshId === null) {
        set({ scratchpad: { meshId: null, text: '', loadError: null, loaded: false } });
        return;
      }
      const requestMeshId = meshId;
      set({ scratchpad: { meshId, text: '', loadError: null, loaded: false } });
      api.getMeshScratchpad(meshId)
        .then((content) => {
          if (!ownsScratchpadWrite(requestMeshId, requestRevision)) return;
          set((state) => ({ scratchpad: { ...state.scratchpad, text: content, loaded: true } }));
        })
        .catch((e) => {
          if (!ownsScratchpadWrite(requestMeshId, requestRevision)) return;
          console.error('Failed to load scratch pad:', e);
          set((state) => ({
            scratchpad: { ...state.scratchpad, loadError: formatError(e) },
          }));
        });
    },

    editScratchpad: (text) => {
      const { scratchpad } = get();
      // An edit against a row we never read would be written blind.
      if (!scratchpad.loaded || scratchpad.meshId === null) return;
      const meshId = scratchpad.meshId;
      set((state) => ({ scratchpad: { ...state.scratchpad, text } }));
      const revision = (scratchpadRevision += 1);
      startSave();
      // Collapse a burst of keystrokes into one write.
      clearScratchpadTimer();
      scratchpadPending = { meshId, text };
      scratchpadTimer = setTimeout(() => {
        scratchpadTimer = null;
        const pending = scratchpadPending;
        if (!pending) return;
        scratchpadPending = null;
        api
          .setMeshScratchpad(pending.meshId, pending.text)
          .then(() => {
            // If the user has since switched Meshes, or typed again, this
            // result belongs to a state they are not looking at.
            if (ownsScratchpadWrite(pending.meshId, revision)) saveSucceeded();
          })
          .catch((e) => {
            if (ownsScratchpadWrite(pending.meshId, revision)) {
              console.error('Failed to save scratch pad:', e);
              saveFailed(e);
            } else {
              console.error('Scratch pad save failed after mesh switch:', e);
            }
          });
      }, SCRATCHPAD_DEBOUNCE_MS);
    },

    flushScratchpad: () => {
      clearScratchpadTimer();
      const pending = scratchpadPending;
      if (!pending) return;
      scratchpadPending = null;
      // Fire-and-forget: the user has already moved on, and the next load
      // reads whatever landed. The save-status path reports the outcome only
      // while it still owns the form, so this rejection is console-only.
      api.setMeshScratchpad(pending.meshId, pending.text).catch((e) => {
        console.error('Failed to flush scratch pad on mesh switch:', e);
      });
    },
  };
});