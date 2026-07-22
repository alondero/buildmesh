/**
 * MeshPropertiesTab — the Probe Panel's Mesh Properties tab (issue #375).
 *
 * This is the *clean* Mesh Properties: pure configuration fields plus the
 * AI context portability helper. All Git-maintenance UI (worktree config,
 * branches, uncommitted changes, branch recovery) is intentionally excluded
 * — that lives behind the dedicated 🌳 Worktree Manager tab, and the
 * uncommitted-changes / PR surface belongs to the 🔍 Agent Changes tab.
 *
 * Fields ported from the legacy `MeshPropertiesPanel.tsx` (deleted in #380;
 * the `AiContextSection` helper moved into this directory in #410):
 *   • Display name (auto-save on blur, syncs the meshStore)
 *   • Directory (read-only — derived from the mesh row)
 *   • AI context portability (delegated to `<AiContextSection>`)
 *   • Default Claude Code model
 *   • Effort level (low/medium/high/xhigh/max)
 *   • Default provider
 *   • Project preset (auto-fill build / run)
 *   • Build / run commands
 *
 * Reactivity model matches the legacy panel: text fields save on blur,
 * selects and the preset picker save on change. The probe's
 * `useProbeContext()` hook drives which mesh is being edited — switching
 * meshes re-runs the load effect.
 */

import { useEffect, useRef, useState, useCallback } from 'react';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { useSaveStatus } from '../../hooks/useSaveStatus';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { AiContextSection } from './AiContextSection';
import { SaveIndicator } from '../shared/SaveIndicator';
import { groupByHarness } from '../../lib/groups';
import {
  checkGhAuth,
  detectMeshProject,
  getAppPreferences,
  getMeshProperties,
  listProviders,
  updateMeshAutopilot,
  updateMeshColumn,
  updateMeshSandbox,
  type ProviderInfo,
} from '../../lib/tauri';
import {
  PROJECT_PRESETS,
  resolvePreset,
  type DetectedProject,
  type ProjectPreset,
} from '../../lib/projectPresets';
import { LoadingState } from '../shared/Spinner';
import { ProbeTabBody } from './ProbeTabBody';
import { Field } from './Field';

// Autopilot Policy (issue #481, PRD #480) — mirrors the backend's
// `update_mesh_autopilot` validation: 1..=8 concurrency, known actions.
const AUTOPILOT_CONCURRENCY_OPTIONS = [1, 2, 3, 4, 5, 6, 7, 8];
const AUTOPILOT_ACTION_OPTIONS = [
  { value: 'draft_pr', label: 'Open draft PR (default)' },
  { value: 'pr', label: 'Open PR ready for review' },
  { value: 'none', label: 'Push only (no PR)' },
];
const DEFAULT_AUTOPILOT_TRIGGER_LABEL = 'buildmesh:run';

const EFFORT_OPTIONS = [
  { value: '', label: 'Not set' },
  { value: 'low', label: 'Low' },
  { value: 'medium', label: 'Medium' },
  { value: 'high', label: 'High' },
  { value: 'xhigh', label: 'XHigh' },
  { value: 'max', label: 'Max' },
];

export function MeshPropertiesTab() {
  const { activeMeshId, activeMeshPath } = useProbeContext();
  const mesh = useMeshStore((s) =>
    activeMeshId !== null ? s.meshesById.get(activeMeshId) : undefined
  );
  const updateMeshName = useMeshStore((s) => s.updateMeshName);
  // Delete Mesh — restored from the legacy `MeshPropertiesPanel` (deleted
  // in #380). The store's `deleteMesh` calls `delete_mesh` and refetches
  // the mesh list; `toggleProbe` closes the probe on success, matching
  // the legacy `closePropertiesPanel()` behaviour.
  const deleteMesh = useMeshStore((s) => s.deleteMesh);
  const toggleProbe = useUIStore((s) => s.toggleProbe);

  const [form, setForm] = useState({
    name: '',
    model: '',
    effort: '',
    buildCommand: '',
    runCommand: '',
    rootBuildCommand: '',
    rootRunCommand: '',
    defaultProvider: '',
sandbox: false,
    autopilotEnabled: false,
    autopilotTriggerLabel: '',
    autopilotConcurrencyLimit: 2,
    autopilotProvider: '',
    autopilotActionOnSuccess: '',
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
  // Delete-Mesh confirm dialog. Ported verbatim from the legacy
  // `MeshPropertiesPanel` (deleted in #380) — the trigger button lives at
  // the bottom of the form, and the dialog shares the
  // `<ConfirmDialog>` shape used by the Worktree Manager tab.
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const mountedRef = useRef(true);

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
  useAsyncEffect((signal) => {
    if (activeMeshId === null || !activeMeshPath) return;
    setLoading(true);
    getMeshProperties(activeMeshId)
      .then((config) => {
        if (signal.aborted) return;
        const folderName = activeMeshPath.split(/[/\\]/).pop() ?? '';
        const resolvedName = config.name || mesh?.name || folderName;
        setForm({
          name: resolvedName,
          model: config.model ?? '',
          effort: config.effort ?? '',
          buildCommand: config.build_command ?? '',
          runCommand: config.run_command ?? '',
          rootBuildCommand: config.root_build_command ?? '',
          rootRunCommand: config.root_run_command ?? '',
          defaultProvider: config.default_provider ?? '',
sandbox: config.sandbox,
          autopilotEnabled: config.autopilot_enabled,
          autopilotTriggerLabel: config.autopilot_trigger_label ?? '',
          autopilotConcurrencyLimit: config.autopilot_concurrency_limit || 2,
          autopilotProvider: config.autopilot_provider ?? '',
          autopilotActionOnSuccess: config.autopilot_action_on_success ?? '',
        });
        setLoading(false);
      })
      .catch(() => {
        if (!signal.aborted) setLoading(false);
      });
    // Dep array intentionally excludes `mesh?.name` (see comment above).
    // `mesh` is captured at effect-run time, which is fine for the
    // fallback chain — the user can rename later via the form itself.
  }, [activeMeshId, activeMeshPath]);

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
  // field) because (a) issue AC says "and/or" — a single global
  // indicator satisfies it, and (b) the tab has 7 auto-save fields,
  // and 7 stacked tiny indicators would compete for the same attention
  // as the form below them. The hook clears the prior error before
  // each save and surfaces the rejection's `.message` on fail.
  const saveStatus = useSaveStatus();

  // Reset the indicator on mesh-switch so a stale "Save failed" from
  // the outgoing mesh doesn't bleed onto the incoming mesh's form. A
  // bare useEffect that tracks `activeMeshId` is sufficient — the
  // hook's own reset() cancels the pending saved→idle timer cleanly.
  useEffect(() => {
    saveStatus.reset();
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
   * (previously swallowed — the issue's "don't swallow failures"
   * complaint). The user's input value is intentionally NOT reverted
   * on a failure (matching the existing `saveSandbox` optimistic rule
   * and the issue AC "keep the field's text").
   *
   * Mesh-switch guard (review finding #1): we capture `activeMeshId`
   * at the moment the IPC starts, then check it on resolve/reject
   * via the ref (NOT the closure-captured prop — that would always
   * equal the value at fire time, never the current one). If the user
   * switched meshes while the IPC was in flight, the result belongs
   * to the OUTGOING mesh, not the one the user is looking at —
   * surfacing "Save failed" or "Saved" on the new mesh would be wrong.
   * Same defensive pattern as `ScratchpadTab.tsx:158-168` (`if
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
        console.error('Mesh property save failed after mesh switch:', e);
        return;
      }
      // Console.error preserves the audit trail in buildmesh.log for
      // the dev / bug-report path; the rendered banner shows the same
      // message to the user so the cause is actionable.
      console.error('Mesh property save failed:', e);
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

  const saveModel = async (value: string) => {
    if (activeMeshId === null) return;
    await wrappedSave(() => updateMeshColumn(activeMeshId, 'model', value || ''));
  };

  const saveEffort = async (value: string) => {
    if (activeMeshId === null || !value) return;
    await wrappedSave(() => updateMeshColumn(activeMeshId, 'effort', value));
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

  // Autopilot Policy save — the five fields persist atomically through one
  // typed command, so every control funnels its *next* form state through
  // this helper (passing the patched object, not reading `form`, avoids
  // saving a stale closure snapshot). Optimistic like `saveSandbox`.
  const saveAutopilot = async (next: typeof form) => {
    if (activeMeshId === null) return;
    await wrappedSave(() =>
      updateMeshAutopilot(
        activeMeshId,
        next.autopilotEnabled,
        next.autopilotTriggerLabel.trim() || null,
        next.autopilotConcurrencyLimit,
        next.autopilotProvider || null,
        next.autopilotActionOnSuccess || null
      )
    );
  };

  const patchAutopilot = async (patch: Partial<typeof form>) => {
    const next = { ...form, ...patch };
    setForm(next);
    await saveAutopilot(next);
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

  // Delete Mesh — ported from the legacy `MeshPropertiesPanel`. The store
  // swallows IPC errors into `state.error` (mirrors the legacy try/catch),
  // so closing the probe is unconditional on a non-throwing deleteMesh.
  // `setShowDeleteConfirm(false)` runs first so the dialog unmounts before
  // the probe closes (avoids a brief double-overlay flash).
  const handleDelete = async () => {
    if (activeMeshId === null) return;
    try {
      await deleteMesh(activeMeshId);
      setShowDeleteConfirm(false);
      toggleProbe();
    } catch (e) {
      console.error('Failed to delete mesh:', e);
    }
  };

  // Without a focused mesh there is nothing to edit. The probe shell
  // already renders a friendlier "no project" empty state, so this is
  // belt-and-braces in case the tab is ever mounted standalone.
  if (activeMeshId === null || !mesh || !activeMeshPath) return null;

  return (
    <ProbeTabBody padding="p-3" className="space-y-4">
      {/* Issue #729 — global SaveIndicator at the top of the tab.
          Renders the current `saveStatus`: "Saving…" mid-write,
          "Saved" after a successful write (auto-clearing), or
          "Save failed: <message>" when the IPC rejects. The banner
          sits above the form so a failed write is visible without
          scrolling; empty / idle state is suppressed so the UI stays
          quiet when nothing is happening. Lifted to a shared primitive
          (issue #813) so `ScratchpadTab` adopts the same vocabulary
          without re-implementing both the state machine and the
          visual. */}
      <SaveIndicator
        status={saveStatus.status}
        error={saveStatus.error}
        onDismiss={saveStatus.reset}
      />
      {loading ? (
        <LoadingState />
      ) : (
        <>
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

          {/* AI context portability — surfaces when the repo has Claude
              context worth mirroring. Stays in Properties because it is
              about project configuration, not Git maintenance. */}
          <AiContextSection
            meshId={activeMeshId}
            meshPath={activeMeshPath}
            isAuthenticated={isGhAuthenticated}
          />

          <Field
            label="Model"
            hint="Claude Code only"
            htmlFor="mesh-prop-model"
          >
            <input
              id="mesh-prop-model"
              type="text"
              value={form.model}
              onChange={(e) => setForm((p) => ({ ...p, model: e.target.value }))}
              onBlur={async (e) => {
                if (!mountedRef.current) return;
                await saveModel(e.target.value);
              }}
              placeholder="e.g., opus-4, sonnet-4"
              className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
            />
          </Field>

          <Field
            label="Effort"
            hint="Claude Code only"
            htmlFor="mesh-prop-effort"
          >
            <select
              id="mesh-prop-effort"
              value={form.effort}
              onChange={async (e) => {
                setForm((p) => ({ ...p, effort: e.target.value }));
                await saveEffort(e.target.value);
              }}
              className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
            >
              {EFFORT_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
          </Field>

          <Field label="Default provider" htmlFor="mesh-prop-provider">
            <select
              id="mesh-prop-provider"
              value={form.defaultProvider}
              onChange={async (e) => {
                setForm((p) => ({ ...p, defaultProvider: e.target.value }));
                await saveDefaultProvider(e.target.value);
              }}
              className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
            >
              <option value="">&lt;Default&gt; ({appWideDefault})</option>
              {/* Issue #575 / ADR-0016 — group the Spawn Options by their
                  `group_key` (== `harness_id`) so the dropdown matches the
                  harness-grouped Spawn Menu shape on every other surface.
                  The native row in each bucket is the clickable harness
                  header; subsequent rows are Proxied children. We collapse
                  a bucket to a plain <option> when the harness has no
                  children so the one-harness config still looks clean.

                  The optgroup label is the native row's `label` (e.g.
                  "Claude Code"), NOT the raw `harness_id` (e.g. "claude")
                  — the harness profile name is the user-facing string.
                  The native row's label is the harness profile's friendly
                  name from `HarnessProfile.name`, populated by
                  `provider_info_for` on the backend. The bucketing is
                  shared with `GroupedProviderMenu` and the mobile
                  `ProviderPicker` via `groupByHarness` (issue #583). */}
              {groupByHarness(providers).map(([harnessId, group]) => {
                if (group.length === 1) {
                  return (
                    <option key={group[0].id} value={group[0].id}>
                      {group[0].label}
                    </option>
                  );
                }
                // Find the native row to use its friendly name as the
                // optgroup label. If the bucket has no native row (e.g.
                // a filter removed it — see code-review finding B2),
                // fall back to the raw `harness_id` rather than the
                // first child's proxied label.
                const native = group.find((p) => !p.is_proxied) ?? group[0];
                return (
                  <optgroup key={harnessId} label={native.label}>
                    {group.map((p) => (
                      <option key={p.id} value={p.id}>
                        {p.is_proxied ? `  ${p.label} (via ${native.label})` : p.label}
                      </option>
                    ))}
                  </optgroup>
                );
              })}
            </select>
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
              Run this mesh's agents inside an OS process sandbox, confining
              filesystem access to the node's worktree.
            </p>
          </div>

          {/* Autopilot Mode (issue #481, PRD #480). The policy fields only
              render while enabled — the section stays one quiet checkbox
              for meshes that never use Autopilot. */}
          <div className="pt-3 border-t border-border-subtle space-y-3">
            <label
              htmlFor="mesh-prop-autopilot"
              className="flex items-center gap-2 text-xs cursor-pointer"
            >
              <input
                id="mesh-prop-autopilot"
                type="checkbox"
                checked={form.autopilotEnabled}
                onChange={async (e) => {
                  await patchAutopilot({ autopilotEnabled: e.target.checked });
                }}
                className="accent-accent-cyan"
              />
              <span className="text-text-primary">Autopilot Mode</span>
            </label>
            <p className="text-xs text-text-muted/70 -mt-2">
              Poll GitHub for labelled issues and auto-spawn isolated agent
              nodes that implement, verify, and raise a PR.
            </p>

            {form.autopilotEnabled && (
              <>
                <Field label="Trigger label" htmlFor="mesh-prop-autopilot-label">
                  <input
                    id="mesh-prop-autopilot-label"
                    type="text"
                    value={form.autopilotTriggerLabel}
                    onChange={(e) =>
                      setForm((p) => ({ ...p, autopilotTriggerLabel: e.target.value }))
                    }
                    onBlur={async (e) => {
                      if (!mountedRef.current) return;
                      await saveAutopilot({
                        ...form,
                        autopilotTriggerLabel: e.target.value,
                      });
                    }}
                    placeholder={DEFAULT_AUTOPILOT_TRIGGER_LABEL}
                    className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary placeholder:text-text-muted/60 placeholder:italic focus:outline-none focus:border-accent-cyan"
                  />
                </Field>

                <Field
                  label="Max concurrent autopilot nodes"
                  htmlFor="mesh-prop-autopilot-concurrency"
                >
                  <select
                    id="mesh-prop-autopilot-concurrency"
                    value={form.autopilotConcurrencyLimit}
                    onChange={async (e) => {
                      await patchAutopilot({
                        autopilotConcurrencyLimit: Number(e.target.value),
                      });
                    }}
                    className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
                  >
                    {AUTOPILOT_CONCURRENCY_OPTIONS.map((n) => (
                      <option key={n} value={n}>
                        {n}
                      </option>
                    ))}
                  </select>
                </Field>

                <Field label="Autopilot provider" htmlFor="mesh-prop-autopilot-provider">
                  <select
                    id="mesh-prop-autopilot-provider"
                    value={form.autopilotProvider}
                    onChange={async (e) => {
                      await patchAutopilot({ autopilotProvider: e.target.value });
                    }}
                    className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
                  >
                    <option value="">&lt;Mesh default&gt;</option>
                    {groupByHarness(providers).map(([harnessId, group]) => {
                      if (group.length === 1) {
                        return (
                          <option key={group[0].id} value={group[0].id}>
                            {group[0].label}
                          </option>
                        );
                      }
                      const native = group.find((p) => !p.is_proxied) ?? group[0];
                      return (
                        <optgroup key={harnessId} label={native.label}>
                          {group.map((p) => (
                            <option key={p.id} value={p.id}>
                              {p.is_proxied
                                ? `  ${p.label} (via ${native.label})`
                                : p.label}
                            </option>
                          ))}
                        </optgroup>
                      );
                    })}
                  </select>
                </Field>

                <Field label="On success" htmlFor="mesh-prop-autopilot-action">
                  <select
                    id="mesh-prop-autopilot-action"
                    value={form.autopilotActionOnSuccess || 'draft_pr'}
                    onChange={async (e) => {
                      await patchAutopilot({
                        autopilotActionOnSuccess: e.target.value,
                      });
                    }}
                    className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
                  >
                    {AUTOPILOT_ACTION_OPTIONS.map((o) => (
                      <option key={o.value} value={o.value}>
                        {o.label}
                      </option>
                    ))}
                  </select>
                </Field>
              </>
            )}
          </div>

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
                    className="text-xs text-accent-cyan hover:text-accent-cyan/80 font-medium transition-colors"
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

          {/* Destructive zone — Delete Mesh (restored from the legacy
              `MeshPropertiesPanel`, deleted in #380). Border-top separates
              it from the config fields above so the destructive action is
              visually distinct. The confirm dialog mirrors the pattern used
              by the Worktree Manager tab and the legacy drawer. Sits where
              the duplicate macOS-only Sandbox toggle used to live, before
              #521 removed that block. */}
          <div className="pt-3 border-t border-border-subtle">
            <button
              type="button"
              onClick={() => setShowDeleteConfirm(true)}
              className="w-full bg-status-error/10 hover:bg-status-error/20 text-status-error text-xs font-medium py-2 rounded-md transition-colors"
            >
              Delete Mesh
            </button>
          </div>
        </>
      )}

      {showDeleteConfirm && mesh && (
        <ConfirmDialog
          title="Delete Mesh"
          message={`Delete "${mesh.name}" and all its agent nodes? This cannot be undone.`}
          confirmLabel="Delete"
          onConfirm={handleDelete}
          onCancel={() => setShowDeleteConfirm(false)}
        />
      )}
    </ProbeTabBody>
  );
}
