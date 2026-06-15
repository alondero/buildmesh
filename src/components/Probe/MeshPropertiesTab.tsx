/**
 * MeshPropertiesTab — the Probe Panel's ⚙️ Mesh Properties tab (issue #375).
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
 *   • Default cwrap model
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

import { useEffect, useRef, useState } from 'react';
import { useMeshStore } from '../../stores/meshStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { AiContextSection } from './AiContextSection';
import {
  checkGhAuth,
  detectMeshProject,
  getMeshProperties,
  listProviders,
  updateMeshField,
  type ProviderInfo,
} from '../../lib/tauri';
import {
  PROJECT_PRESETS,
  resolvePreset,
  type DetectedProject,
  type ProjectPreset,
} from '../../lib/projectPresets';

const EFFORT_OPTIONS = [
  { value: '', label: 'Not set' },
  { value: 'low', label: 'Low' },
  { value: 'medium', label: 'Medium' },
  { value: 'high', label: 'High' },
  { value: 'xhigh', label: 'XHigh' },
  { value: 'max', label: 'Max' },
];

export function MeshPropertiesTab() {
  const { activeMeshId, activePath } = useProbeContext();
  const mesh = useMeshStore((s) =>
    activeMeshId !== null ? s.meshesById.get(activeMeshId) : undefined
  );
  const updateMeshName = useMeshStore((s) => s.updateMeshName);

  const [form, setForm] = useState({
    name: '',
    model: '',
    effort: '',
    buildCommand: '',
    runCommand: '',
    defaultProvider: '',
  });
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [detected, setDetected] = useState<DetectedProject | null>(null);
  const [loading, setLoading] = useState(true);
  // `MeshHealth` does not surface `gh auth` status (it's about branch /
  // drift / dirty, not the GitHub CLI login state), but `<AiContextSection>`
  // needs to know whether to render the "Run gh auth login first" prompt
  // versus the "Make portable" button. A single `checkGhAuth` round-trip
  // per mesh is cheap and avoids pulling in the heavy `useMeshGitStatus`
  // hook (which also fetches the file list and repo-ness).
  const [isGhAuthenticated, setIsGhAuthenticated] = useState(false);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // Fetch the provider list once on mount; the catalogue is static for the
  // life of the session, so a re-fetch per mesh switch would be wasted work.
  useEffect(() => {
    listProviders()
      .then(setProviders)
      .catch(() => setProviders([]));
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
  // running detection against `activePath` would silently miss the
  // mesh's actual project type whenever a node is focused.
  const meshRoot = mesh?.path ?? null;
  useAsyncEffect((signal) => {
    if (activeMeshId === null || !meshRoot) return;
    setDetected(null);
    detectMeshProject(meshRoot)
      .then((d) => {
        if (!signal.aborted) setDetected(d);
      })
      .catch(() => {
        if (!signal.aborted) setDetected(null);
      });
  }, [activeMeshId, meshRoot]);

  // Load the mesh's saved config every time the active mesh changes.
  // Intentionally depends on `activeMeshId` only — re-firing on
  // `mesh?.name` would clobber the user's in-flight edits to Model,
  // Build, Run, etc. with the just-saved values (the `updateMeshName`
  // call mutates `meshesById` and would otherwise re-trigger this
  // effect on every name save). The "config.name → mesh.name → folder
  // name" fallback chain runs at mount only; the user can still rename
  // later via the Name field.
  useAsyncEffect((signal) => {
    if (activeMeshId === null || !activePath) return;
    setLoading(true);
    getMeshProperties(activeMeshId)
      .then((config) => {
        if (signal.aborted) return;
        const folderName = activePath.split(/[/\\]/).pop() ?? '';
        const resolvedName = config.name || mesh?.name || folderName;
        setForm({
          name: resolvedName,
          model: config.model ?? '',
          effort: config.effort ?? '',
          buildCommand: config.build_command ?? '',
          runCommand: config.run_command ?? '',
          defaultProvider: config.default_provider ?? '',
        });
        setLoading(false);
      })
      .catch(() => {
        if (!signal.aborted) setLoading(false);
      });
    // Dep array intentionally excludes `mesh?.name` (see comment above).
    // `mesh` is captured at effect-run time, which is fine for the
    // fallback chain — the user can rename later via the form itself.
  }, [activeMeshId, activePath]);

  // Keep `mountedRef` ONLY for the blur handlers' "save-after-unmount"
  // guard. The 3 IPC effects above use `useAsyncEffect`'s AbortSignal
  // (PR #390, issue #349) — see the hook for the rationale.
  // (No further effects below this line use mountedRef.)

  // Auto-save helpers — each returns a promise so callers can await then
  // refetch. The legacy panel reused `git.refresh()` after every save to
  // keep the sidebar drift badge in sync; the new tab skips the explicit
  // refresh because the `useMeshHealth` cache the sidebar consumes is
  // already refetched by its own GIT_CHANGED / focus invalidate path
  // (and `mesh.toml` writes don't change git health anyway).
  const saveName = async (name: string) => {
    if (activeMeshId === null) return;
    if (name !== mesh?.name) {
      await updateMeshName(activeMeshId, name);
    }
  };

  const saveModel = async (value: string) => {
    if (activeMeshId === null) return;
    await updateMeshField(activeMeshId, 'agent', 'model', value || '');
  };

  const saveEffort = async (value: string) => {
    if (activeMeshId === null || !value) return;
    await updateMeshField(activeMeshId, 'agent', 'effort', value);
  };

  const saveBuildCommand = async (value: string) => {
    if (activeMeshId === null) return;
    await updateMeshField(activeMeshId, 'build', 'command', value);
  };

  const saveRunCommand = async (value: string) => {
    if (activeMeshId === null) return;
    await updateMeshField(activeMeshId, 'run', 'command', value);
  };

  const saveDefaultProvider = async (value: string) => {
    if (activeMeshId === null) return;
    await updateMeshField(activeMeshId, 'agent', 'default_provider', value);
  };

  const applyPreset = async (preset: ProjectPreset) => {
    if (activeMeshId === null) return;
    setForm((p) => ({ ...p, buildCommand: preset.build, runCommand: preset.run }));
    await Promise.all([
      updateMeshField(activeMeshId, 'build', 'command', preset.build),
      updateMeshField(activeMeshId, 'run', 'command', preset.run),
    ]);
  };

  const applyPresetById = async (id: string) => {
    const preset = resolvePreset(id, detected?.node_scripts);
    if (!preset) return;
    await applyPreset(preset);
  };

  // Without a focused mesh there is nothing to edit. The probe shell
  // already renders a friendlier "no project" empty state, so this is
  // belt-and-braces in case the tab is ever mounted standalone.
  if (activeMeshId === null || !mesh || !activePath) return null;

  return (
    <div className="p-4 space-y-4">
      {loading ? (
        <p className="text-xs text-text-muted text-center py-8">Loading…</p>
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
              className="w-full bg-bg-overlay border border-border-subtle rounded px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
            />
          </Field>

          <Field label="Directory" htmlFor="mesh-prop-dir">
            <input
              id="mesh-prop-dir"
              type="text"
              value={activePath}
              readOnly
              className="w-full bg-bg-surface border border-border-subtle rounded px-2 py-1.5 text-xs text-text-muted font-mono"
            />
          </Field>

          {/* AI context portability — surfaces when the repo has Claude
              context worth mirroring. Stays in Properties because it is
              about project configuration, not Git maintenance. */}
          <AiContextSection
            meshId={activeMeshId}
            meshPath={activePath}
            isAuthenticated={isGhAuthenticated}
          />

          <Field
            label="Model"
            hint="cwrap only"
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
              className="w-full bg-bg-overlay border border-border-subtle rounded px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
            />
          </Field>

          <Field
            label="Effort"
            hint="cwrap only"
            htmlFor="mesh-prop-effort"
          >
            <select
              id="mesh-prop-effort"
              value={form.effort}
              onChange={async (e) => {
                setForm((p) => ({ ...p, effort: e.target.value }));
                await saveEffort(e.target.value);
              }}
              className="w-full bg-bg-overlay border border-border-subtle rounded px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
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
              className="w-full bg-bg-overlay border border-border-subtle rounded px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
            >
              <option value="">&lt;Default&gt; (Anthropic)</option>
              {providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                </option>
              ))}
            </select>
          </Field>

          <Field label="Project preset" htmlFor="mesh-prop-preset">
            <select
              id="mesh-prop-preset"
              value=""
              onChange={(e) => {
                if (e.target.value) void applyPresetById(e.target.value);
              }}
              className="w-full bg-bg-overlay border border-border-subtle rounded px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
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
                <div className="mt-2 flex items-start gap-2 bg-accent-cyan/5 border border-accent-cyan/30 rounded px-2 py-1.5">
                  <span className="text-xs text-text-secondary flex-1">
                    Looks like a{' '}
                    <span className="text-text-primary">{detected.label}</span>{' '}
                    project.
                  </span>
                  <button
                    type="button"
                    onClick={() => void applyPresetById(detected.preset_id!)}
                    className="text-xs text-accent-cyan hover:text-accent-cyan/80 font-medium"
                  >
                    Apply preset
                  </button>
                </div>
              )}
          </Field>

          <Field label="Build command" htmlFor="mesh-prop-build">
            <input
              id="mesh-prop-build"
              type="text"
              value={form.buildCommand}
              onChange={(e) => setForm((p) => ({ ...p, buildCommand: e.target.value }))}
              onBlur={async (e) => {
                if (!mountedRef.current) return;
                await saveBuildCommand(e.target.value);
              }}
              placeholder="npm run build"
              className="w-full bg-bg-overlay border border-border-subtle rounded px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
            />
          </Field>

          <Field label="Run command" htmlFor="mesh-prop-run">
            <input
              id="mesh-prop-run"
              type="text"
              value={form.runCommand}
              onChange={(e) => setForm((p) => ({ ...p, runCommand: e.target.value }))}
              onBlur={async (e) => {
                if (!mountedRef.current) return;
                await saveRunCommand(e.target.value);
              }}
              placeholder="npm run dev"
              className="w-full bg-bg-overlay border border-border-subtle rounded px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan"
            />
          </Field>
        </>
      )}
    </div>
  );
}

interface FieldProps {
  label: string;
  htmlFor: string;
  hint?: string;
  children: React.ReactNode;
}

/**
 * A small wrapper that standardises the label/control rhythm. Kept local
 * because it only ever appears inside this tab. The `htmlFor`/`id` wiring
 * is what lets `getByLabelText` resolve the form control in tests — and
 * what lets click-to-focus the label work for keyboard users.
 */
function Field({ label, htmlFor, hint, children }: FieldProps) {
  return (
    <div>
      <label htmlFor={htmlFor} className="block text-xs text-text-muted mb-1">
        {label}
        {hint && <span className="text-text-muted/60"> ({hint})</span>}
      </label>
      {children}
    </div>
  );
}
