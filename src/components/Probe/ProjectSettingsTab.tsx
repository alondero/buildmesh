import { PROBE_TAB_DEFINITIONS } from '../../lib/probeContext';
import { useProbeResource, type ProbeLoadContext } from '../../lib/probeResource';
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
 *     commands, or strategy. Delete Mesh lives in an explicitly labelled danger
 *     zone that states its impact and recovery story *before* the confirmation
 *     (the old inline red button said neither).
 *
 * Fields carried over from the legacy `MeshPropertiesPanel` (deleted in
 * #380; `AiContextSection` moved into the Probe directory in #410):
 *   - Display name (auto-save on blur, syncs the meshStore)
 *   - Directory (read-only, derived from the mesh row)
 *   - AI context portability (delegated to `<AiContextSection>`)
 *   - Default provider, sandbox toggle (developer-gated, issue #2034)
 *   - Project preset (auto-fill build / run)
 *   - Build / run commands, plus the per-context root overrides
 *
 * Legacy Autopilot policy was removed from the application; Circuit settings
 * live in the Circuits destination.
 *
 * **This component holds no settings state.** The per-Mesh editing session —
 * the form drafts, the worktree-strategy drafts, the load, the auto-save
 * writes, the save-status machine and the mesh-switch guard that drops a write
 * whose Mesh the user has left — lives in `stores/meshSettingsStore.ts`
 * (issue #2242). This file keeps only what is genuinely local to the render:
 * the provider catalogue, the app-wide default label, the sandbox
 * availability probe, the `gh auth` flag, and the delete confirmation.
 *
 * Reactivity model matches the legacy panel: text fields save on blur,
 * selects, radios, and toggles save on change. Two error channels, both
 * pre-existing: the destination-level `saveStatus` banner (issue #729) for
 * the text/select fields, and a section-local message for the worktree
 * strategy controls, whose failure text names the column that refused the
 * write. Neither reverts the user's input on failure.
 */

import { useEffect, useState, useCallback } from 'react';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useUIStore } from '../../stores/uiStore';
import { useMeshSettingsStore, meshSettingsReady } from '../../stores/meshSettingsStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { usePoolChanged } from '../../hooks/usePoolChanged';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import {
  concernsMesh,
  subscribeWorktreeDirectoryChanges,
} from '../../lib/streams/worktreeDirectoryStream';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { AiContextSection } from './AiContextSection';
import { SaveIndicator } from '../shared/SaveIndicator';
import { SpawnOptionPicker, useSpawnOptionLabel } from '../Providers/SpawnOptionPicker';
import {
  checkGhAuth,
  getAppPreferences,
  listProviders,
  sandboxDevModeEnabled,
  type ProviderInfo,
} from '../../lib/tauri';
import { PROJECT_PRESETS, resolvePreset } from '../../lib/projectPresets';
import { ErrorState, LoadingState } from '../shared/Spinner';
import { ProbeTabBody } from './ProbeTabBody';
import { ProbeScopeNote, ProbeSection } from './ProbeSection';
import { Field } from './Field';
import { WorktreeStrategySection } from './WorktreeStrategySection';

export async function loadProjectSettings({ meshId, meshPath, fallbackName }: ProbeLoadContext) {
  if (meshId !== null && meshPath) {
    await useMeshSettingsStore.getState().load({ meshId, meshPath, fallbackName });
  }
  return null;
}

export function ProjectSettingsTab() {
  const { activeMeshId, activeMeshPath } = useProbeContext();
  const mesh = useMeshStore((s) =>
    activeMeshId !== null ? s.meshesById.get(activeMeshId) : undefined
  );
  // Delete Mesh. The store's `deleteMesh` calls `delete_mesh` and refetches
  // the mesh list; `toggleProbe` closes the probe on success, matching the
  // legacy `closePropertiesPanel()` behaviour.
  const deleteMesh = useMeshStore((s) => s.deleteMesh);
  const toggleProbe = useUIStore((s) => s.toggleProbe);

  // ── The editing session (issue #2242) ──────────────────────────────────
  const form = useMeshSettingsStore((s) => s.form);
  const detected = useMeshSettingsStore((s) => s.detected);
  const loading = useMeshSettingsStore((s) => s.loading);
  const settingsReady = useMeshSettingsStore(s => meshSettingsReady(s, activeMeshId));
  const loadError = useMeshSettingsStore((s) => s.loadError);
  const status = useMeshSettingsStore((s) => s.status);
  const statusError = useMeshSettingsStore((s) => s.statusError);
  const editForm = useMeshSettingsStore((s) => s.editForm);
  const save = useMeshSettingsStore((s) => s.save);
  const applyPreset = useMeshSettingsStore((s) => s.applyPreset);
  const resetSaveStatus = useMeshSettingsStore((s) => s.resetSaveStatus);
  const refreshWorktreeDirectory = useMeshSettingsStore((s) => s.refreshWorktreeDirectory);
  const refreshPoolCountFor = useMeshSettingsStore((s) => s.refreshPoolCount);

  // Whether the experimental agent sandbox is available in this process
  // (issue #2034). The backend ignores `meshes.sandbox` entirely unless
  // BUILDMESH_SANDBOX=1, so shipping builds must not offer the toggle at all —
  // a control that appears to work while the spawn path discards it is worse
  // than no control. Defaults to false so the toggle never flashes in.
  const [sandboxDevMode, setSandboxDevMode] = useState(false);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  // App-wide default provider id (from preferences.json). Drives the
  // `<Default> ({X})` label on the dropdown's first option so Adam can
  // see at a glance whether `<Default>` would route through Claude Code
  // or a proxied provider. Initial state mirrors the post-#538
  // `resolve_default_provider` fallback (`"claude"`) so the first render
  // doesn't flash a stale `"anthropic"` label before the async fetch
  // resolves.
  const [appWideDefault, setAppWideDefault] = useState<string>('claude');
  const resource = useProbeResource(PROBE_TAB_DEFINITIONS.properties.load, {
    meshId: activeMeshId, meshPath: activeMeshPath, fallbackName: mesh?.name,
  }, `${activeMeshId}:${activeMeshPath}`);
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

  // Fetch the provider list once on mount; the catalogue is mostly static
  // for the life of the session, so a re-fetch per mesh switch would be
  // wasted work — but it CAN change when the user adds or removes a custom
  // provider in App Settings, so the hook below re-fires this fetch on the
  // `provider-list-changed` event.
  //
  // A payload that is not a list is treated as an empty catalogue, the same as
  // a rejected read. `useSpawnOptionLabel` maps the array on every render, so a
  // non-array that survives here crashes the whole destination on the next
  // render for any other reason — an async store update, a mesh switch. The
  // boundary that fetches the payload is the right place to normalise it.
  const refreshProviders = useCallback(() => {
    listProviders()
      .then((next) => setProviders(Array.isArray(next) ? next : []))
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

  // Sandbox availability is a property of the running process, not of the
  // selected Mesh, so it is read once on mount rather than inside the
  // per-mesh load. A failed read leaves the toggle hidden: the backend
  // would ignore `meshes.sandbox` in that state anyway, so showing the control
  // would advertise a setting that does nothing.
  useEffect(() => {
    let cancelled = false;
    sandboxDevModeEnabled()
      .then((enabled) => { if (!cancelled) setSandboxDevMode(enabled === true); })
      .catch(() => { if (!cancelled) setSandboxDevMode(false); });
    return () => { cancelled = true; };
  }, []);

  useEffect(() => { setShowDeleteConfirm(false); }, [activeMeshId]);

  // Re-resolve when the directory config changes outside this destination
  // (e.g. the app-wide default edited in Settings while the probe is
  // open). Skips events for other meshes; `null` (app default moved)
  // refreshes any mesh. The store owns the reads and drops a response that
  // lands after the user has switched meshes.
  //
  // `worktree-directory-changed` has exactly one subscriber in the app
  // (issue #2531) — the owner module
  // `lib/streams/worktreeDirectoryStream`, which owns the "is this about
  // MY mesh?" rule.
  useEffect(() => {
    if (activeMeshId === null) return;
    let cancelled = false;
    const release = subscribeWorktreeDirectoryChanges((change) => {
      if (!concernsMesh(change, activeMeshId)) return;
      if (cancelled) return;
      void refreshWorktreeDirectory();
    });
    return () => {
      cancelled = true;
      release();
    };
  }, [activeMeshId, refreshWorktreeDirectory]);

  // Single refresh body for the warm-pool readiness badge — mount,
  // mesh-switch, and every `pool-count-changed` event. Stable closure via
  // `useCallback([activeMeshId])` so `usePoolChanged` doesn't tear down and
  // re-attach its listener on every render (the dep-array contract
  // `usePoolChanged` documents). A null mesh clears the badge; errors are
  // swallowed (keep stale value) so a transient SQLite hiccup doesn't blank
  // it — same recovery rule as the config-load path.
  const refreshPoolCount = useCallback(() => {
    refreshPoolCountFor(activeMeshId);
  }, [activeMeshId, refreshPoolCountFor]);

  useEffect(() => { refreshPoolCount(); }, [refreshPoolCount]);

  // Cross-component invalidation: re-fetch on every `pool-count-changed`
  // event from the Rust pool service. The payload (mesh_id) is currently
  // ignored — the badge re-fetches unconditionally on any mesh's change
  // (one extra O(1) COUNT query per event, which is fine).
  usePoolChanged(refreshPoolCount);

  const applyPresetById = async (id: string) => {
    const preset = resolvePreset(id, detected?.node_scripts);
    if (!preset) return;
    await applyPreset(preset);
  };

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
          Renders the session's `status`: "Saving…" mid-write, "Saved"
          after a successful write (auto-clearing), or "Save failed:
          <message>" when the IPC rejects. The banner sits above the
          sections so a failed write is visible without scrolling; empty /
          idle state is suppressed so the UI stays quiet when nothing is
          happening. Lifted to a shared primitive (issue #813) so
          `ScratchpadTab` adopts the same vocabulary without
          re-implementing both the state machine and the visual. */}
      <SaveIndicator
        status={status}
        error={statusError}
        onDismiss={resetSaveStatus}
      />
      {loadError !== null ? (
        <div>
          <ErrorState title="Couldn't load project settings" detail={loadError} />
          <button type="button" className="text-xs text-text-secondary hover:text-text-primary"
            onClick={() => void resource.reload()}>Retry</button>
        </div>
      ) : loading || !settingsReady ? (
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
                onChange={(e) => editForm({ name: e.target.value })}
                onBlur={(e) => void save('name', e.target.value)}
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
                onSelect={(next) => void save('defaultProvider', next ?? '')}
              />
            </Field>

            {/* Sandbox toggle (#528 Windows restricted token / #497 macOS
                Seatbelt). Experimental and developer-gated (issue #2034):
                rendered only when the backend reports BUILDMESH_SANDBOX=1, so
                a release never presents it. The help text states the real
                per-platform behaviour rather than promising worktree
                confinement everywhere — Windows denies no filesystem access
                (#542), Linux has no backend (#828), and WSL is not contained.
                The write is optimistic: reverting a checkbox the user just
                clicked is more confusing than an unchanged value. */}
            {sandboxDevMode && (
              <div>
                <label
                  htmlFor="mesh-prop-sandbox"
                  className="flex items-center gap-2 text-xs text-text-muted cursor-pointer"
                >
                  <input
                    id="mesh-prop-sandbox"
                    type="checkbox"
                    checked={form.sandbox}
                    onChange={(e) => void save('sandbox', e.target.checked)}
                    className="accent-accent-cyan"
                  />
                  <span className="text-text-primary">
                    Sandbox agent processes (experimental)
                  </span>
                </label>
                <p className="mt-1 text-xs text-text-muted">
                  Experimental, and weaker than the label suggests. macOS
                  confines the agent to its worktree via Seatbelt; Windows
                  restricts the process but does <em>not</em> deny filesystem
                  reads or writes; Linux and WSL launches are not contained at
                  all. If the sandbox cannot be set up on macOS the spawn is
                  refused rather than run unconfined.
                </p>
              </div>
            )}
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
                onChange={(e) => editForm({ buildCommand: e.target.value })}
                onBlur={(e) => void save('buildCommand', e.target.value)}
                placeholder="e.g., npm run build — type to override, or pick a preset above"
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary placeholder:text-text-muted placeholder:italic focus:outline-none focus:border-accent-cyan"
              />
            </Field>

            <Field label="Run command" htmlFor="mesh-prop-run" hint="custom">
              <input
                id="mesh-prop-run"
                type="text"
                value={form.runCommand}
                onChange={(e) => editForm({ runCommand: e.target.value })}
                onBlur={(e) => void save('runCommand', e.target.value)}
                placeholder="e.g., npm run dev — type to override, or pick a preset above"
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary placeholder:text-text-muted placeholder:italic focus:outline-none focus:border-accent-cyan"
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
                onChange={(e) => editForm({ rootBuildCommand: e.target.value })}
                onBlur={(e) => void save('rootBuildCommand', e.target.value)}
                placeholder="e.g., cargo build --workspace — run from the mesh root"
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary placeholder:text-text-muted placeholder:italic focus:outline-none focus:border-accent-cyan"
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
                onChange={(e) => editForm({ rootRunCommand: e.target.value })}
                onBlur={(e) => void save('rootRunCommand', e.target.value)}
                placeholder="e.g., npm run lint --workspaces — run from the mesh root"
                className="w-full bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary placeholder:text-text-muted placeholder:italic focus:outline-none focus:border-accent-cyan"
              />
            </Field>
          </ProbeSection>

          {/* Worktree strategy — moved here from the Worktree Manager tab
              (issue #1460). "Where do new agent worktrees get cut, and from
              which ref?" is a strategy decision, and it was previously
              reachable only by entering a maintenance surface whose other
              controls delete branches. The section reads the editing session
              itself (issue #2242), so the thirteen props it used to be handed
              are gone. */}
          <ProbeSection
            title="Worktree strategy"
            testId="project-settings-worktree-strategy"
            description="How new agent worktrees are created and where they start from. Repository cleanup lives in the Repository destination."
          >
            <WorktreeStrategySection />
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