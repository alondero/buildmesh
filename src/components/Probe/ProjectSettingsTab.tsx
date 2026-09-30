/**
 * ProjectSettingsTab — the Probe Panel's Project Settings destination
 * (issue #1460; formerly `MeshPropertiesTab`).
 *
 * This is the **configuration** half of the old Worktree Manager / Mesh
 * Properties pair. Issue #1460's premise was that those two surfaces mixed
 * preferences, strategy, maintenance, and project deletion in one place, so
 * the risk level of any given control was unclear and changing an ordinary
 * setting meant entering a maintenance-heavy surface. The split:
 *
 *   • **Project Settings** (this file) — identity and directory, agent
 *     runtime and harness defaults, build and run, and worktree strategy
 *     (use-worktree, base ref, mode, warm pool, worktree directory). The
 *     worktree-strategy block moved here from the Worktree Manager, because
 *     "where do new agent worktrees get cut" is a strategy decision, not a
 *     maintenance action.
 *   • **Repository** (`RepositoryTab`) — health, recovery, branch/worktree
 *     cleanup, remote-tracking prune. Nothing there writes configuration.
 *
 * Sections are the point of this rewrite, not decoration: each `ProbeSection`
 * heading tells the user whether the controls under it are identity, runtime,
 * commands, or strategy. Delete Mesh lives in an explicitly labelled danger
 * zone that states its impact and recovery story *before* the confirmation
 * (the old inline red button said neither).
 *
 * Fields carried over from the legacy `MeshPropertiesPanel` (deleted in
 * #380; `AiContextSection` moved into the Probe directory in #410):
 *   - Display name (auto-save on blur, syncs the meshStore)
 *   - Directory (read-only, derived from the mesh row)
 *   - AI context portability (delegated to `<AiContextSection>`)
 *   - Default provider, sandbox toggle
 *   - Project preset (auto-fill build / run)
 *   - Build / run commands, plus the per-context root overrides
 *
 * Legacy Autopilot policy was removed from the application; Circuit settings
 * live in the Circuits destination.
 *
 * Reactivity model matches the legacy panel: text fields save on blur,
 * selects, radios, and toggles save on change. Two error channels, both
 * pre-existing: the destination-level `saveStatus` banner (issue #729) for
 * the text/select fields, and a section-local message for the worktree
 * strategy controls, whose failure text names the column that refused the
 * write. Neither reverts the user's input on failure.
 */

import { useEffect, useRef, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useUIStore } from '../../stores/uiStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { usePoolChanged } from '../../hooks/usePoolChanged';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { useSaveStatus } from '../../hooks/useSaveStatus';
import { WORKTREE_DIR_CHANGED_EVENT } from '../../lib/events';
import { formatError } from '../../lib/errorUtils';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { AiContextSection } from './AiContextSection';
import { SaveIndicator } from '../shared/SaveIndicator';
import { SpawnOptionPicker, useSpawnOptionLabel } from '../Providers/SpawnOptionPicker';
import {
  checkGhAuth,
  detectMeshProject,
  getAppPreferences,
  getMeshProperties,
  getWarmPoolCount,
  getWorktreeDirectoryConfig,
  listProviders,
  updateMeshColumn,
  updateMeshPoolSize,
  updateMeshSandbox,
  updateMeshUseWorktree,
  updateMeshWorktreeDirectory,
  updateWorktreeBaseRef,
  type AppPreferences,
  type ProviderInfo,
  type WorktreeDirectoryConfig,
} from '../../lib/tauri';
import {
  PROJECT_PRESETS,
  resolvePreset,
  type DetectedProject,
  type ProjectPreset,
} from '../../lib/projectPresets';
import { getEffectiveWorktreeDir } from '../../lib/paths';
import { LoadingState } from '../shared/Spinner';
import { ProbeTabBody } from './ProbeTabBody';
import { ProbeScopeNote, ProbeSection } from './ProbeSection';
import { Field } from './Field';
import {
  WorktreeStrategySection,
  formToWireBaseRef,
  wireToFormBaseRef,
  wireToFormMode,
  type BaseRefForm,
  type WorktreeModeForm,
} from './WorktreeStrategySection';

export function ProjectSettingsTab() {
  const { activeMeshId, activeMeshPath } = useProbeContext();
  const mesh = useMeshStore((s) =>
    activeMeshId !== null ? s.meshesById.get(activeMeshId) : undefined
  );
  const updateMeshName = useMeshStore((s) => s.updateMeshName);
  // Delete Mesh. The store's `deleteMesh` calls `delete_mesh` and refetches
  // the mesh list; `toggleProbe` closes the probe on success, matching the
  // legacy `closePropertiesPanel()` behaviour.
  const deleteMesh = useMeshStore((s) => s.deleteMesh);
  const toggleProbe = useUIStore((s) => s.toggleProbe);

  const [form, setForm] = useState({
    name: '',
    buildCommand: '',
    runCommand: '',
    rootBuildCommand: '',
    rootRunCommand: '',
    defaultProvider: '',
    sandbox: false,
  });
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [detected, setDetected] = useState<DetectedProject | null>(null);
  // App-wide default provider id (from preferences.json). Drives the
  // `<Default> ({X})` label on the dropdown's first option so Adam can
  // see at a glance whether `<Default>` would route through Claude Code
  // or a proxied provider. Initial state mirrors the post-#538
  // `resolve_default_provider` fallback (`"claude"`) so the first render
  // doesn't flash a stale `"anthropic"` label before the async fetch
  // resolves.
  const [appWideDefault, setAppWideDefault] = useState<string>('claude');
  const [loading, setLoading] = useState(true);
  // `MeshHealth` does not surface `gh auth` status (it's about branch /
  // drift / dirty, not the GitHub CLI login state), but `<AiContextSection>`
  // needs to know whether to render the "Run gh auth login first" prompt
  // versus the "Make portable" button. A single `checkGhAuth` round-trip
  // per mesh is cheap and avoids pulling in the heavy `useMeshGitStatus`
  // hook (which also fetches the file list and repo-ness).
  const [isGhAuthenticated, setIsGhAuthenticated] = useState(false);
  // Delete-Mesh confirm dialog. The trigger lives in the danger zone; the
  // dialog uses the shared `<ConfirmDialog>` shape.
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const mountedRef = useRef(true);

  // ── Worktree strategy state (moved from the Worktree Manager tab,
  //    issue #1460). The three radio/toggle fields initialise to the legacy
  //    `MeshPropertiesPanel` defaults (use a worktree, start a fresh
  //    session, branched mode); the load effect below replaces them with
  //    the persisted values from the wire.
  const [useWorktree, setUseWorktree] = useState(true);
  const [baseRef, setBaseRef] = useState<BaseRefForm>('fresh');
  const [worktreeMode, setWorktreeMode] = useState<WorktreeModeForm>('branched');
  // Pre-spawn Worktree Pool size (issue #611). `0` = pool off, `1..=5` =
  // target. The toggle's `checked` is derived from `preSpawnPoolSize > 0`;
  // the size input clamps to 1..5 and is disabled when the toggle is off.
  // The pool worker (`services::warm_pool`) reads this column on startup +
  // after each claim, draining excess and filling up to this target.
  const [preSpawnPoolSize, setPreSpawnPoolSize] = useState(0);
  // Issue #1519: per-Mesh worktree directory override draft (`''` = inherit)
  // plus the inherited effective value for display. Hydrated from
  // `getMeshProperties` + `getAppPreferences`; the effective display also
  // refreshes from `getWorktreeDirectoryConfig` so the backend precedence
  // rule (not a TS re-spelling) is authoritative.
  const [worktreeDirectory, setWorktreeDirectory] = useState('');
  const [worktreeDirEffective, setWorktreeDirEffective] = useState('');
  const [worktreeDirAppDefault, setWorktreeDirAppDefault] = useState<string | null>(null);
  // Live pre-spawn pool *ready* count for the active mesh. Source of
  // truth is `db::count_available_warm_for_mesh` on the Rust side; the
  // badge re-fetches on mount, on mesh switch, and on every
  // `pool-count-changed` event (which fires from try_claim, prewarm_one,
  // drain_excess_warm_entries, update_mesh_pool_size, and
  // reconcile_on_startup). `null` = first fetch in flight (don't show
  // a stale "0/N" while we don't know yet).
  const [poolCount, setPoolCount] = useState<number | null>(null);
  // Section-local save failure for the worktree strategy controls. Kept
  // apart from `saveStatus` because these messages name the column that
  // refused the write, which a single destination-level banner cannot do.
  const [strategyError, setStrategyError] = useState<string | null>(null);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // Fetch the provider list once on mount; the catalogue is mostly static
  // for the life of the session, so a re-fetch per mesh switch would be
  // wasted work — but it CAN change when the user adds or removes a custom
  // provider in App Settings, so the hook below re-fires this fetch on the
  // `provider-list-changed` event.
  const refreshProviders = useCallback(() => {
    listProviders()
      .then(setProviders)
      .catch(() => setProviders([]));
  }, []);

  useEffect(() => { refreshProviders(); }, [refreshProviders]);
  useProviderListInvalidation(refreshProviders);

  // Fetch the app-wide default provider id once. The label is purely
  // informational (it tells Adam what `<Default>` would route to), so
  // a single fetch on mount is enough — `setAppDefaultProvider` is a
  // rare action that lives in Settings, and the user re-opens the
  // tab to see its effect.
  useEffect(() => {
    getAppPreferences()
      .then((prefs) => {
        // Empty string is "no override" per the backend's
        // `preferences::default_provider` filter — fall through to the
        // post-#538 `claude` fallback so the label matches what
        // `+`-click would actually spawn.
        const value = prefs.default_provider?.trim();
        setAppWideDefault(value && value.length > 0 ? value : 'claude');
      })
      .catch(() => {
        // Swallow — the initial 'claude' default is a sensible fallback;
        // the spawn path resolves it the same way.
      });
  }, []);

  // Lightweight `gh auth status` probe per active mesh. The result is
  // local to the section that needs it, so we don't need to share it via
  // a store or a global cache.
  useAsyncEffect((signal) => {
    if (activeMeshId === null) {
      setIsGhAuthenticated(false);
      return;
    }
    checkGhAuth()
      .then((ok) => {
        if (!signal.aborted) setIsGhAuthenticated(ok);
      })
      .catch(() => {
        if (!signal.aborted) setIsGhAuthenticated(false);
      });
  }, [activeMeshId]);

  // Project-type detection: drives the "Looks like an X project" hint and
  // which preset gets the ✓ marker in the dropdown. Runs against the
  // MESH ROOT, not the focused node's working directory — a Worktree
  // Node's path is the worktree subdir (often missing Cargo.toml /
  // package.json if the worktree only carries the agent's edits), so
  // running detection against `activePath` (the focused-node path) would
  // silently miss the mesh's actual project type whenever a node is
  // focused.
  useAsyncEffect((signal) => {
    if (activeMeshId === null || !activeMeshPath) return;
    setDetected(null);
    detectMeshProject(activeMeshPath)
      .then((d) => {
        if (!signal.aborted) setDetected(d);
      })
      .catch(() => {
        if (!signal.aborted) setDetected(null);
      });
  }, [activeMeshId, activeMeshPath]);

  // Load the mesh's saved config every time the active mesh changes.
  // Intentionally depends on `activeMeshId` only — re-firing on
  // `mesh?.name` would clobber the user's in-flight edits to Model,
  // Build, Run, etc. with the just-saved values (the `updateMeshName`
  // call mutates `meshesById` and would otherwise re-trigger this
  // effect on every name save). The "config.name → mesh.name → folder
  // name" fallback chain runs at mount only; the user can still rename
  // later via the Name field.
  //
  // Applies a loaded directory config to the form state. Shared by the
  // orchestrated mount loader below (guarded by its AbortSignal) and the
  // `worktree-directory-changed` listener further down (guarded by its own
  // `cancelled` flag) so a Settings change elsewhere refreshes this tab
  // instead of leaving a stale inherited value until remount (issue #1519).
  // Declared before the loader so the loader can call it in one pass.
  const applyWorktreeDirConfig = useCallback(
    (
      config: { worktree_directory?: string | null },
      prefs: AppPreferences | null,
      dirConfig: WorktreeDirectoryConfig | null,
    ) => {
      const meshDir = config.worktree_directory?.trim() ?? '';
      setWorktreeDirectory(meshDir);
      const appDir = (prefs?.worktree_directory?.trim() ?? '') || null;
      setWorktreeDirAppDefault(appDir);
      setWorktreeDirEffective(
        dirConfig?.effective_directory ??
          getEffectiveWorktreeDir(activeMeshPath ?? '', meshDir, appDir),
      );
    },
    [activeMeshPath],
  );

  // One orchestrated load per mesh selection (issue #1460). `getMeshProperties`
  // is called ONCE and its single result feeds the form, the worktree
  // strategy, and the directory resolution. The pre-split tab fetched it from
  // two effects with only one of them under `loading`, so the destination
  // could render the previous project's worktree-directory override for a
  // frame after `loading` flipped.
  //
  // Dep discipline is inherited from the Worktree Manager tab (issue #451):
  // the form state is deliberately NOT a dependency, or every save would
  // re-fire the load and overwrite the user's in-flight edits.
  useAsyncEffect(
    (signal) => {
      if (activeMeshId === null || !activeMeshPath) return;
      setLoading(true);
      Promise.all([
        getMeshProperties(activeMeshId),
        // The two secondary reads are individually best-effort: a failure to
        // resolve preferences or the directory config must degrade the
        // "Effective:" display, never blank the whole form.
        getAppPreferences().catch(() => null),
        getWorktreeDirectoryConfig(activeMeshId).catch(() => null),
      ])
        .then(([config, prefs, dirConfig]) => {
          if (signal.aborted) return;
          const folderName = activeMeshPath.split(/[/\\]/).pop() ?? '';
          const resolvedName = config.name || mesh?.name || folderName;
          setForm({
            name: resolvedName,
            buildCommand: config.build_command ?? '',
            runCommand: config.run_command ?? '',
            rootBuildCommand: config.root_build_command ?? '',
            rootRunCommand: config.root_run_command ?? '',
            defaultProvider: config.default_provider ?? '',
            sandbox: config.sandbox,
          });
          setUseWorktree(config.use_worktree);
          setBaseRef(wireToFormBaseRef(config.base_ref));
          setWorktreeMode(wireToFormMode(config.worktree_mode));
          setPreSpawnPoolSize(config.pre_spawn_pool_size);
          // Issue #1519 — the directory override and its resolved effective
          // path hydrate in the same pass, so nothing is left showing the
          // previous project's value.
          applyWorktreeDirConfig(config, prefs, dirConfig);
          setLoading(false);
        })
        .catch(() => {
          if (!signal.aborted) setLoading(false);
        });
      // Dep array intentionally excludes `mesh?.name` (see comment above).
      // `mesh` is captured at effect-run time, which is fine for the
      // fallback chain — the user can rename later via the form itself.
    },
    [activeMeshId, activeMeshPath, applyWorktreeDirConfig],
  );

  // Keep `mountedRef` ONLY for the blur handlers' "save-after-unmount"
  // guard. The 3 IPC effects above use `useAsyncEffect`'s AbortSignal
  // (PR #390, issue #349) — see the hook for the rationale.
  // (No further effects below this line use mountedRef.)

  // Auto-save helpers — each returns a promise so callers can await then
  // refetch. The legacy panel reused `git.refresh()` after every save to
  // keep the sidebar drift badge in sync; the new tab skips the explicit
  // refresh because the `useMeshHealth` cache the sidebar consumes is
  // already refetched by its own GIT_CHANGED / focus invalidate path
  // (and `meshes` column writes don't change git health anyway).
  //
  // Issue #729 — every save goes through `saveStatus` so the top-of-tab
  // "Saving… / Saved / Save failed" indicator tracks the most-recent
  // write. We keep one hook instance for the tab (rather than one per
  // field) because a single global indicator satisfies the requirement and
  // the tab has many auto-save fields; stacked tiny indicators would
  // compete for the same attention as the form below them. The hook clears
  // the prior error before each save and surfaces the rejection's
  // `.message` on fail.
  const saveStatus = useSaveStatus();

  // Reset the indicator on mesh-switch so a stale "Save failed" from
  // the outgoing mesh doesn't bleed onto the incoming mesh's form. A
  // bare useEffect that tracks `activeMeshId` is sufficient — the
  // hook's own reset() cancels the pending saved→idle timer cleanly.
  // `saveStatus.reset()` is bound to the SaveIndicator hook instance,
  // which is stable for the lifetime of the consumer — the rule can't
  // see through the hook's return type, so the disable is justified.
  useEffect(() => {
    saveStatus.reset();
    setStrategyError(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeMeshId, saveStatus.reset]);

  // Ref mirror of `activeMeshId` so the IPC-`.then`/`.catch` in
  // `wrappedSave` can read the CURRENT mesh id at resolve time, not
  // the closure-captured value from the render where the IPC was
  // fired. Without this, a slow save from the outgoing mesh would
  // still see `activeMeshId === saveMeshId` (both are the stale value
  // from the previous render) and surface its error on the new mesh
  // — review finding #1.
  const activeMeshIdRef = useRef(activeMeshId);
  useEffect(() => {
    activeMeshIdRef.current = activeMeshId;
  }, [activeMeshId]);

  /**
   * Internal save wrapper. Drives the state machine on every IPC
   * outcome, and on rejection surfaces the rejection's `.message`
   * (previously swallowed). The user's input value is intentionally NOT
   * reverted on a failure (matching the existing `saveSandbox` optimistic
   * rule and the "keep the field's text" requirement).
   *
   * Mesh-switch guard (review finding #1): we capture `activeMeshId`
   * at the moment the IPC starts, then check it on resolve/reject
   * via the ref (NOT the closure-captured prop — that would always
   * equal the value at fire time, never the current one). If the user
   * switched meshes while the IPC was in flight, the result belongs
   * to the OUTGOING mesh, not the one the user is looking at —
   * surfacing "Save failed" or "Saved" on the new mesh would be wrong.
   * Same defensive pattern as `ScratchpadTab.tsx` (`if
   * (pending.meshId === activeMeshId)`). The `saveStatus.reset()` in
   * the `useEffect([activeMeshId])` above already clears the
   * indicator on switch, but it cannot stop the IPC's later
   * `.then`/`.catch` from re-applying state — this guard is the
   * second line of defence.
   */
  const wrappedSave = async (op: () => Promise<void>) => {
    const saveMeshId = activeMeshIdRef.current;
    saveStatus.start();
    try {
      await op();
      if (activeMeshIdRef.current !== saveMeshId) return;
      saveStatus.success();
    } catch (e) {
      if (activeMeshIdRef.current !== saveMeshId) {
        // Audit trail only — the user is looking at a different
        // mesh's form; the indicator stays clean.
        console.error('Project setting save failed after mesh switch:', e);
        return;
      }
      // Console.error preserves the audit trail in buildmesh.log for
      // the dev / bug-report path; the rendered banner shows the same
      // message to the user so the cause is actionable.
      console.error('Project setting save failed:', e);
      saveStatus.fail(e);
    }
  };

  const saveName = async (name: string) => {
    if (activeMeshId === null) return;
    if (name === mesh?.name) {
      // No-op write: skip the IPC and avoid a false "Saved" flicker
      // that would briefly show success for a no-op.
      return;
    }
    await wrappedSave(() => updateMeshName(activeMeshId, name));
  };

  const saveBuildCommand = async (value: string) => {
    if (activeMeshId === null) return;
    await wrappedSave(() => updateMeshColumn(activeMeshId, 'build_command', value));
  };

  const saveRunCommand = async (value: string) => {
    if (activeMeshId === null) return;
    await wrappedSave(() => updateMeshColumn(activeMeshId, 'run_command', value));
  };

  // Per-context commands (issue #802). Optional — a blank value clears the
  // column so the Root Node falls back to build_command / run_command.
  const saveRootBuildCommand = async (value: string) => {
    if (activeMeshId === null) return;
    await wrappedSave(() => updateMeshColumn(activeMeshId, 'root_build_command', value));
  };

  const saveRootRunCommand = async (value: string) => {
    if (activeMeshId === null) return;
    await wrappedSave(() => updateMeshColumn(activeMeshId, 'root_run_command', value));
  };

  const saveDefaultProvider = async (value: string) => {
    if (activeMeshId === null) return;
    await wrappedSave(() => updateMeshColumn(activeMeshId, 'default_provider', value));
  };

  // Sandbox toggle (#497 / #498). Optimistic, matching the "do not revert on
  // failure" rule of the other binary controls — reverting a checkbox the user
  // just clicked is more confusing than an unchanged value. The flag is OS-
  // agnostic at the DB/UI layer; the OS-specific spawn policy is decided in
  // `spawn_environment::wrap` on the backend.
  const saveSandbox = async (value: boolean) => {
    if (activeMeshId === null) return;
    setForm((p) => ({ ...p, sandbox: value }));
    await wrappedSave(() => updateMeshSandbox(activeMeshId, value));
  };

  const applyPreset = async (preset: ProjectPreset) => {
    if (activeMeshId === null) return;
    setForm((p) => ({ ...p, buildCommand: preset.build, runCommand: preset.run }));
    await wrappedSave(async () => {
      await Promise.all([
        updateMeshColumn(activeMeshId, 'build_command', preset.build),
        updateMeshColumn(activeMeshId, 'run_command', preset.run),
      ]);
    });
  };

  const applyPresetById = async (id: string) => {
    const preset = resolvePreset(id, detected?.node_scripts);
    if (!preset) return;
    await applyPreset(preset);
  };

  /**
   * Mesh-switch guard for the worktree-strategy saves, which is the same rule
   * `wrappedSave` applies with two differences: the failure lands in the
   * section-local `strategyError` (it must name the column that refused the
   * write), and the control is NOT reverted on failure (the "form mirrors
   * user intent" rule).
   *
   * `saveMeshId` is captured at the moment the IPC starts and compared against
   * the ref on settle. Without this, a write for the outgoing project would
   * paint its error over the incoming project's form — and, in
   * `handleChangeWorktreeDirectory`, would write the outgoing project's
   * directory config into the incoming project's fields.
   *
   * Returns whether the write succeeded so the caller can gate its own
   * post-save refresh on the same boundary.
   */
  const strategySave = useCallback(
    async (saveMeshId: number, op: () => Promise<unknown>, failurePrefix: string) => {
      try {
        await op();
        return true;
      } catch (e) {
        if (activeMeshIdRef.current !== saveMeshId) {
          // Audit trail only — the user is looking at a different project's
          // form, so the section stays clean.
          console.error('Worktree strategy save failed after mesh switch:', e);
          return false;
        }
        setStrategyError(`${failurePrefix}: ${formatError(e)}`);
        return false;
      }
    },
    [],
  );

  // ── Worktree-strategy save handlers (moved from the Worktree Manager
  //    tab, issue #1460). Each: (1) update form state optimistically,
  //    (2) clear any prior save error, (3) call the typed wrapper through
  //    `strategySave` so the mesh-switch boundary is respected, (4) on
  //    reject, surface the error inline WITHOUT reverting the form. The
  //    "do not revert" rule matches the legacy panel — for a binary control
  //    like a checkbox, reverting would silently undo the user's click and
  //    they would have no idea why.
  const handleToggleUseWorktree = useCallback(
    async (next: boolean) => {
      const saveMeshId = activeMeshId;
      if (saveMeshId === null) return;
      setUseWorktree(next);
      setStrategyError(null);
      await strategySave(
        saveMeshId,
        () => updateMeshUseWorktree(saveMeshId, next),
        'Failed to update use_worktree',
      );
    },
    [activeMeshId, strategySave],
  );

  const handleChangeBaseRef = useCallback(
    async (next: BaseRefForm) => {
      const saveMeshId = activeMeshId;
      if (saveMeshId === null) return;
      setBaseRef(next);
      setStrategyError(null);
      await strategySave(
        saveMeshId,
        () => updateWorktreeBaseRef(saveMeshId, formToWireBaseRef(next)),
        'Failed to update base_ref',
      );
    },
    [activeMeshId, strategySave],
  );

  const handleChangeWorktreeMode = useCallback(
    async (next: WorktreeModeForm) => {
      const saveMeshId = activeMeshId;
      if (saveMeshId === null) return;
      setWorktreeMode(next);
      setStrategyError(null);
      await strategySave(
        saveMeshId,
        () => updateMeshColumn(saveMeshId, 'worktree_mode', next),
        'Failed to update worktree_mode',
      );
    },
    [activeMeshId, strategySave],
  );

  // Pre-spawn pool size save (issue #611). Single handler for both the
  // toggle (which sends 0 or 1) and the size input (which sends 1..5);
  // the backend enforces the `0..=5` invariant and rejects otherwise.
  // Mirrors the other save handlers' "do not revert on save failure"
  // rule — if the user shrinks the pool to 5 and the backend rejects,
  // the form keeps their typed value so the failure message is
  // actionable.
  const handleChangePoolSize = useCallback(
    async (next: number) => {
      const saveMeshId = activeMeshId;
      if (saveMeshId === null) return;
      const clamped = Math.max(0, Math.min(5, Math.trunc(next) || 0));
      setPreSpawnPoolSize(clamped);
      setStrategyError(null);
      await strategySave(
        saveMeshId,
        () => updateMeshPoolSize(saveMeshId, clamped),
        'Failed to update pool size',
      );
    },
    [activeMeshId, strategySave],
  );

  // Per-Mesh worktree directory save (issue #1519). `''`/blank clears the
  // override so the Mesh inherits the app default; anything else stores the
  // trimmed raw input (no shell/`~` expansion). Absolute paths are validated
  // backend-side for same-environment (native vs WSL) with an actionable
  // error. Like the other handlers, the form keeps the typed value on
  // failure. On success, refresh the effective display from the backend so
  // the inherited value can't drift from the precedence rule — and re-check
  // the mesh boundary after each await, because that refresh is the one place
  // this handler writes three fields from a second project's response.
  const handleChangeWorktreeDirectory = useCallback(
    async (next: string) => {
      const saveMeshId = activeMeshId;
      if (saveMeshId === null) return;
      setWorktreeDirectory(next);
      setStrategyError(null);
      const trimmed = next.trim();
      const saved = await strategySave(
        saveMeshId,
        () => updateMeshWorktreeDirectory(saveMeshId, trimmed === '' ? null : trimmed),
        'Failed to update worktree directory',
      );
      if (!saved || activeMeshIdRef.current !== saveMeshId) return;
      try {
        const cfg = await getWorktreeDirectoryConfig(saveMeshId);
        if (activeMeshIdRef.current !== saveMeshId) return;
        setWorktreeDirEffective(cfg.effective_directory);
        setWorktreeDirAppDefault(cfg.app_directory);
        setWorktreeDirectory(cfg.mesh_directory?.trim() ?? '');
      } catch {
        // Keep the optimistic form on config-refresh failure — the save
        // itself succeeded; the effective display refreshes on next load.
        if (activeMeshIdRef.current !== saveMeshId) return;
        setWorktreeDirEffective(
          getEffectiveWorktreeDir(activeMeshPath ?? '', trimmed, worktreeDirAppDefault),
        );
      }
    },
    [activeMeshId, activeMeshPath, worktreeDirAppDefault, strategySave],
  );

  // Re-resolve when the directory config changes outside this destination
  // (e.g. the app-wide default edited in Settings while the probe is
  // open). Skips events for other meshes; `null` (app default moved)
  // refreshes any mesh. Guarded by its own `cancelled` flag (the mount loader
  // has the AbortSignal for the same race).
  useEffect(() => {
    if (activeMeshId === null) return;
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    listen<number | null>(WORKTREE_DIR_CHANGED_EVENT, (event) => {
      const affected = event.payload;
      if (affected !== null && affected !== undefined && affected !== activeMeshId) return;
      if (cancelled) return;
      Promise.all([
        getMeshProperties(activeMeshId),
        getAppPreferences().catch(() => null),
        getWorktreeDirectoryConfig(activeMeshId).catch(() => null),
      ])
        .then(([config, prefs, dirConfig]) => {
          if (!cancelled) applyWorktreeDirConfig(config, prefs, dirConfig);
        })
        .catch(() => {
          // Swallow — keep the existing form state (see above).
        });
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [activeMeshId, applyWorktreeDirConfig]);

  // Single refresh body for the warm-pool readiness badge — mount,
  // mesh-switch, and every `pool-count-changed` event. Stable closure via
  // `useCallback([activeMeshId])` so `usePoolChanged` doesn't tear down and
  // re-attach its listener on every render (the dep-array contract
  // `usePoolChanged` documents). Errors are swallowed (keep stale value)
  // so a transient SQLite hiccup doesn't blank the badge — same recovery
  // rule as the config-load path.
  const refreshPoolCount = useCallback(() => {
    if (activeMeshId === null) return;
    getWarmPoolCount(activeMeshId)
      .then((n) => setPoolCount(n))
      .catch(() => {
        /* keep stale value */
      });
  }, [activeMeshId]);

  useAsyncEffect(
    (signal) => {
      if (activeMeshId === null) {
        setPoolCount(null);
        return;
      }
      getWarmPoolCount(activeMeshId)
        .then((n) => {
          if (!signal.aborted) setPoolCount(n);
        })
        .catch(() => {
          /* keep stale value */
        });
    },
    [activeMeshId],
  );

  // Cross-component invalidation: re-fetch on every `pool-count-changed`
  // event from the Rust pool service. The payload (mesh_id) is currently
  // ignored — the badge re-fetches unconditionally on any mesh's change
  // (one extra O(1) COUNT query per event, which is fine).
  usePoolChanged(refreshPoolCount);

  // Delete Mesh. The store swallows IPC errors into `state.error` (mirrors
  // the legacy try/catch and matches the rest of the meshStore surface), so
  // a try/catch around `deleteMesh` was unreachable. Issue #1247 made
  // `deleteMesh` return a boolean so callers can gate their follow-up on
  // the real outcome: on failure the mesh survives and the user stays in
  // the same destination to retry; on success `setShowDeleteConfirm(false)`
  // unmounts the confirm dialog first so it disappears before
  // `toggleProbe()` flips the probe off (avoids a brief double-overlay
  // flash).
  const handleDelete = async () => {
    if (activeMeshId === null) return;
    const ok = await deleteMesh(activeMeshId);
    if (!ok) return;
    setShowDeleteConfirm(false);
    toggleProbe();
  };

  // Agent Node count for the Delete Mesh confirmation (issue #1460 AC4). The
  // danger zone says the consequence before the user commits, and "its 3
  // agent nodes" is a scope statement while "its agent nodes" is a vague one —
  // the user cannot tell a two-node project from a thirty-node one. The
  // normalized store (#1384) keeps `nodeIds` as the canonical
  // `(mesh_id, position)` order, so count through it rather than
  // `Object.values(nodesById)`. Returns a primitive, so this re-renders only
  // when the count itself changes.
  const agentNodeCount = useAgentNodeStore((s) => {
    if (activeMeshId === null) return 0;
    let count = 0;
    for (const id of s.nodeIds) {
      const node = s.nodesById[id];
      if (node?.mesh_id === activeMeshId) count += 1;
    }
    return count;
  });

  // Human label for the app-wide default the `<Default>` inherit row would
  // route to (falls back to the raw id when the row isn't resolvable, e.g. a
  // Launch Configuration whose harness is hidden on this host).
  const appWideDefaultLabel = useSpawnOptionLabel(providers, appWideDefault) ?? appWideDefault;

  // Without a focused mesh there is nothing to edit. The probe shell
  // already renders a friendlier "no project" empty state, so this is
  // belt-and-braces in case the tab is ever mounted standalone.
  if (activeMeshId === null || !mesh || !activeMeshPath) return null;

  return (
    <ProbeTabBody padding="p-3" className="space-y-4">
      {/* Issue #729 — global SaveIndicator at the top of the destination.
          Renders the current `saveStatus`: "Saving…" mid-write, "Saved"
          after a successful write (auto-clearing), or "Save failed:
          <message>" when the IPC rejects. The banner sits above the
          sections so a failed write is visible without scrolling; empty /
          idle state is suppressed so the UI stays quiet when nothing is
          happening. Lifted to a shared primitive (issue #813) so
          `ScratchpadTab` adopts the same vocabulary without
          re-implementing both the state machine and the visual. */}
      <SaveIndicator
        status={saveStatus.status}
        error={saveStatus.error}
        onDismiss={saveStatus.reset}
      />
      {loading ? (
        <LoadingState />
      ) : (
        <>
          <ProbeSection
            title="General"
            testId="project-settings-general"
            description="How this project is named and where it lives."
          >
            <Field label="Name" htmlFor="mesh-prop-name">
              <input
                id="mesh-prop-name"
                type="text"
                value={form.name}
                onChange={(e) => setForm((p) => ({ ...p, name: e.target.value }))}
                onBlur={async (e) => {
                  if (!mountedRef.current) return;
                  await saveName(e.target.value);
                }}
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
              />
            </Field>

            <Field label="Directory" htmlFor="mesh-prop-dir">
              <input
                id="mesh-prop-dir"
                type="text"
                value={activeMeshPath}
                readOnly
                className="w-full bg-bg-surface border border-border-subtle rounded-md px-2 py-1.5 text-xs text-text-secondary font-mono"
              />
            </Field>
            {/* Issue #1460 — path semantics are stated, not inferred. The
                Directory field is the project ROOT; a focused agent's
                worktree is a different path, and every setting on this
                destination applies to the root. */}
            <ProbeScopeNote>
              This is the project root, not the focused agent&apos;s worktree.
            </ProbeScopeNote>
          </ProbeSection>

          {/* AI context portability — surfaces when the repo has Claude
              context worth mirroring. Kept in Project Settings because it is
              about project configuration, not Git maintenance. It renders
              its own status-tinted card, so it sits between sections rather
              than nested inside one. */}
          <AiContextSection
            meshId={activeMeshId}
            meshPath={activeMeshPath}
            isAuthenticated={isGhAuthenticated}
          />

          <ProbeSection
            title="Agent runtime"
            testId="project-settings-runtime"
            description="Defaults applied when a new agent starts in this project."
          >
            <Field label="Default provider" htmlFor="mesh-prop-provider">
              {/* ADR-0016 — reuse the exact Spawn Menu so this picker's
                  options match every other spawn surface: native harness
                  parents with a `›` configuration submenu. Selecting a saved
                  Launch Configuration stores its id; the backend resolver
                  (`launch_configurations::resolve`) treats a configuration id
                  as a valid `default_provider` selection. */}
              <SpawnOptionPicker
                id="mesh-prop-provider"
                ariaLabel="Default provider"
                providers={providers}
                value={form.defaultProvider || null}
                unsetLabel={`<Default> (${appWideDefaultLabel})`}
                unsetValue=""
                onSelect={async (next) => {
                  const value = next ?? '';
                  setForm((p) => ({ ...p, defaultProvider: value }));
                  await saveDefaultProvider(value);
                }}
              />
            </Field>

            {/* Sandbox toggle (#498 Windows AppContainer / #497 macOS Seatbelt).
                Default-on lands once the native spawn path is validated; until
                then this persists the per-mesh preference. */}
            <div>
              <label
                htmlFor="mesh-prop-sandbox"
                className="flex items-center gap-2 text-xs text-text-muted cursor-pointer"
              >
                <input
                  id="mesh-prop-sandbox"
                  type="checkbox"
                  checked={form.sandbox}
                  onChange={async (e) => {
                    const next = e.target.checked;
                    setForm((p) => ({ ...p, sandbox: next }));
                    await saveSandbox(next);
                  }}
                  className="accent-accent-cyan"
                />
                <span className="text-text-primary">Sandbox agent processes</span>
              </label>
              <p className="mt-1 text-xs text-text-muted/70">
                Run this mesh&apos;s agents inside an OS process sandbox, confining
                filesystem access to the node&apos;s worktree.
              </p>
            </div>
          </ProbeSection>

          <ProbeSection
            title="Build and run"
            testId="project-settings-build-run"
            description="Commands used to build and run this project."
          >
            <Field label="Project preset" htmlFor="mesh-prop-preset">
              <select
                id="mesh-prop-preset"
                value=""
                onChange={(e) => {
                  if (e.target.value) void applyPresetById(e.target.value);
                }}
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
              >
                <option value="">Choose a preset to fill Build/Run…</option>
                {PROJECT_PRESETS.map((p) => (
                  <option key={p.id} value={p.id}>
                    {detected?.preset_id === p.id
                      ? `✓ ${p.label} (detected)`
                      : p.label}
                  </option>
                ))}
              </select>
              {detected?.preset_id &&
                !form.buildCommand.trim() &&
                !form.runCommand.trim() && (
                  <div className="mt-2 flex items-start gap-2 bg-accent-cyan/5 border border-accent-cyan/30 rounded-md px-2 py-1.5">
                    <span className="text-xs text-text-secondary flex-1">
                      Looks like a{' '}
                      <span className="text-text-primary">{detected.label}</span>{' '}
                      project.
                    </span>
                    <button
                      type="button"
                      onClick={() => void applyPresetById(detected.preset_id!)}
                      className="text-xs text-text-secondary hover:text-text-primary font-medium transition-colors"
                    >
                      Apply preset
                    </button>
                  </div>
                )}
            </Field>

            <Field label="Build command" htmlFor="mesh-prop-build" hint="custom">
              <input
                id="mesh-prop-build"
                type="text"
                value={form.buildCommand}
                onChange={(e) => setForm((p) => ({ ...p, buildCommand: e.target.value }))}
                onBlur={async (e) => {
                  if (!mountedRef.current) return;
                  await saveBuildCommand(e.target.value);
                }}
                placeholder="e.g., npm run build — type to override, or pick a preset above"
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary placeholder:text-text-muted/60 placeholder:italic focus:outline-none focus:border-accent-cyan"
              />
            </Field>

            <Field label="Run command" htmlFor="mesh-prop-run" hint="custom">
              <input
                id="mesh-prop-run"
                type="text"
                value={form.runCommand}
                onChange={(e) => setForm((p) => ({ ...p, runCommand: e.target.value }))}
                onBlur={async (e) => {
                  if (!mountedRef.current) return;
                  await saveRunCommand(e.target.value);
                }}
                placeholder="e.g., npm run dev — type to override, or pick a preset above"
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary placeholder:text-text-muted/60 placeholder:italic focus:outline-none focus:border-accent-cyan"
              />
            </Field>

            {/* Per-context build/run overrides (issue #802). A Root Node runs
                these instead of the commands above; leaving them blank falls
                back to Build / Run command in every context. */}
            <Field
              label="Root build command"
              htmlFor="mesh-prop-root-build"
              hint="optional — falls back to build command"
            >
              <input
                id="mesh-prop-root-build"
                type="text"
                value={form.rootBuildCommand}
                onChange={(e) => setForm((p) => ({ ...p, rootBuildCommand: e.target.value }))}
                onBlur={async (e) => {
                  if (!mountedRef.current) return;
                  await saveRootBuildCommand(e.target.value);
                }}
                placeholder="e.g., cargo build --workspace — run from the mesh root"
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary placeholder:text-text-muted/60 placeholder:italic focus:outline-none focus:border-accent-cyan"
              />
            </Field>

            <Field
              label="Root run command"
              htmlFor="mesh-prop-root-run"
              hint="optional — falls back to run command"
            >
              <input
                id="mesh-prop-root-run"
                type="text"
                value={form.rootRunCommand}
                onChange={(e) => setForm((p) => ({ ...p, rootRunCommand: e.target.value }))}
                onBlur={async (e) => {
                  if (!mountedRef.current) return;
                  await saveRootRunCommand(e.target.value);
                }}
                placeholder="e.g., npm run lint --workspaces — run from the mesh root"
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary placeholder:text-text-muted/60 placeholder:italic focus:outline-none focus:border-accent-cyan"
              />
            </Field>
          </ProbeSection>

          {/* Worktree strategy — moved here from the Worktree Manager tab
              (issue #1460). "Where do new agent worktrees get cut, and from
              which ref?" is a strategy decision, and it was previously
              reachable only by entering a maintenance surface whose other
              controls delete branches. */}
          <ProbeSection
            title="Worktree strategy"
            testId="project-settings-worktree-strategy"
            description="How new agent worktrees are created and where they start from. Repository cleanup lives in the Repository destination."
          >
            <WorktreeStrategySection
              useWorktree={useWorktree}
              baseRef={baseRef}
              worktreeMode={worktreeMode}
              preSpawnPoolSize={preSpawnPoolSize}
              poolCount={poolCount}
              worktreeDirectory={worktreeDirectory}
              worktreeDirEffective={worktreeDirEffective}
              error={strategyError}
              onToggleUseWorktree={handleToggleUseWorktree}
              onChangeBaseRef={handleChangeBaseRef}
              onChangeWorktreeMode={handleChangeWorktreeMode}
              onChangePoolSize={handleChangePoolSize}
              onChangeWorktreeDirectory={handleChangeWorktreeDirectory}
            />
          </ProbeSection>

          {/* Danger zone — Delete Mesh. Issue #1460 requires the destructive
              action to be a *labelled* zone that states its impact AND its
              recovery story before the confirmation; the old inline red
              button at the foot of an undifferentiated form stated neither,
              which is precisely the "unclear risk level" the issue set out
              to fix. The `ConfirmDialog` is the shared pattern, so the
              confirmation states the scope a second time at the point of
              commitment. */}
          <ProbeSection
            title="Danger zone"
            tone="danger"
            testId="project-settings-danger-zone"
            description="Deleting the project stops its agents and forgets it. Files already on disk are left alone, and the directory can be re-added as a new project."
          >
            <button
              type="button"
              onClick={() => setShowDeleteConfirm(true)}
              data-testid="delete-mesh"
              className="w-full bg-status-error/10 hover:bg-status-error/20 text-status-error text-xs font-medium py-2 rounded-md transition-colors"
            >
              Delete Mesh
            </button>
          </ProbeSection>
        </>
      )}

      {showDeleteConfirm && mesh && (
        <ConfirmDialog
          title="Delete Mesh"
          // Names the project, the exact node count, and what survives — the
          // two facts a user needs to decide (issue #1460 AC4). A zero-node
          // project reads "and its 0 agent nodes", which is still accurate and
          // avoids a branch the reader has to interpret.
          message={`Delete "${mesh.name}" and its ${agentNodeCount} agent node${agentNodeCount === 1 ? '' : 's'}? Their sessions end and this project disappears from Buildmesh; the directory at ${mesh.path} stays on disk and can be added again as a new project. This cannot be undone.`}
          confirmLabel="Delete"
          onConfirm={handleDelete}
          onCancel={() => setShowDeleteConfirm(false)}
        />
      )}
    </ProbeTabBody>
  );
}
