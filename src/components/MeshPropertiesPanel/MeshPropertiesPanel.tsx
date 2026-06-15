import { useEffect, useRef, useState } from 'react';
import { useMeshStore } from '../../stores/meshStore';
import { useMeshPropertiesStore } from '../../stores/meshPropertiesStore';
import { useUIStore } from '../../stores/uiStore';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { UncommittedChangesSection } from './UncommittedChangesSection';
import { AiContextSection } from './AiContextSection';
import { useMeshGitStatus } from '../../hooks/useMeshGitStatus';
import { listProviders, ProviderInfo } from '../../lib/tauri';
import {
  PROJECT_PRESETS,
  resolvePreset,
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

const BASEREF_OPTIONS = [
  { value: 'fresh', label: 'Fresh — start new session (origin/<default>)' },
  { value: 'head', label: 'Head — resume last session (HEAD)' },
];

// Must agree with `DEFAULT_WORKTREE_MODE` in `src-tauri/src/agent/spawn.rs`.
export const DEFAULT_WORKTREE_MODE = 'branched';

const WORKTREE_MODE_OPTIONS = [
  { value: 'branched', label: 'Branched — actual git branch per worktree (default)' },
  { value: 'detached', label: 'Detached — detached HEAD worktree' },
];

export function MeshPropertiesPanel() {
  const propertiesPanelMeshId = useUIStore((s) => s.propertiesPanelMeshId);
  const closePropertiesPanel = useUIStore((s) => s.closePropertiesPanel);
  const toggleFileExplorer = useUIStore((s) => s.toggleFileExplorer);
  const mesh = useMeshStore((s) =>
    propertiesPanelMeshId != null ? s.meshesById.get(propertiesPanelMeshId) : undefined
  );
  const updateMeshName = useMeshStore((s) => s.updateMeshName);
  const deleteMesh = useMeshStore((s) => s.deleteMesh);
  const selectMesh = useMeshStore((s) => s.selectMesh);

  // The headless intake (issue #283): one store owns load / save(field, value)
  // / applyPreset for every panel auto-save. The component holds only the
  // form's editing buffer (cursor positions, mid-typing values) and binds
  // its blur/change handlers to `save`. Component pattern: `await save(...)`
  // then optionally `git?.refresh()` — the git side-effect stays here, not
  // in the store, because it's a render concern of the panel layout.
  const config = useMeshPropertiesStore((s) => s.config);
  const detected = useMeshPropertiesStore((s) => s.detected);
  const loading = useMeshPropertiesStore((s) => s.loading);
  const loadProperties = useMeshPropertiesStore((s) => s.load);
  const saveProperty = useMeshPropertiesStore((s) => s.save);
  const applyPresetStore = useMeshPropertiesStore((s) => s.applyPreset);
  const resetProperties = useMeshPropertiesStore((s) => s.reset);

  const [form, setForm] = useState({
    name: '',
    model: '',
    effort: '',
    useWorktree: true,
    baseRef: 'fresh',
    worktreeMode: DEFAULT_WORKTREE_MODE,
    buildCommand: '',
    runCommand: '',
    defaultProvider: '',
  });
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const mountedRef = useRef(true);

  const git = useMeshGitStatus(mesh?.path ?? null);

  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);

  useEffect(() => {
    listProviders().then(setProviders).catch(() => {});
  }, []);

  // Drive the store's load when the panel opens for a (new) mesh; reset
  // when it closes so a re-open starts from `loading=true` rather than
  // flashing the previous mesh's values.
  useEffect(() => {
    if (propertiesPanelMeshId == null || !mesh?.path) {
      resetProperties();
      return;
    }
    loadProperties(propertiesPanelMeshId, mesh.path);
  }, [propertiesPanelMeshId, mesh?.path, loadProperties, resetProperties]);

  // Mirror the loaded config into the form's editing buffer once it arrives.
  // The form is a controlled-input mirror, not the source of truth — the
  // store is authoritative.
  useEffect(() => {
    if (!config) return;
    const folderName = mesh?.path.split(/[/\\]/).pop() ?? '';
    const resolvedName = config.name || mesh?.name || folderName;
    setForm({
      name: resolvedName,
      model: config.model ?? '',
      effort: config.effort ?? '',
      useWorktree: config.use_worktree,
      baseRef: config.base_ref === 'HEAD' ? 'head' : 'fresh',
      worktreeMode: config.worktree_mode ?? DEFAULT_WORKTREE_MODE,
      buildCommand: config.build_command ?? '',
      runCommand: config.run_command ?? '',
      defaultProvider: config.default_provider ?? '',
    });
  }, [config, mesh?.name, mesh?.path]);

  // The name auto-save still routes through `useMeshStore.updateMeshName`
  // because the sidebar reads `mesh.name` from there and would otherwise
  // miss the optimistic update.
  const saveName = async (name: string) => {
    if (name !== mesh?.name) {
      await updateMeshName(propertiesPanelMeshId!, name);
    }
  };

  const applyPresetById = async (id: string) => {
    const preset: ProjectPreset | undefined = resolvePreset(id, detected?.node_scripts);
    if (!preset) return;
    setForm((p) => ({ ...p, buildCommand: preset.build, runCommand: preset.run }));
    await applyPresetStore(preset);
    git?.refresh();
  };

  const handleDelete = async () => {
    try {
      await deleteMesh(propertiesPanelMeshId!);
      closePropertiesPanel();
    } catch (e) {
      console.error('Failed to delete mesh:', e);
    }
  };

  const handleViewDiff = () => {
    if (propertiesPanelMeshId == null || !mesh?.path) return;
    // Select the mesh first: SessionView closes any file explorer whose mesh
    // isn't the selected one, so opening for a non-selected mesh would no-op.
    selectMesh(propertiesPanelMeshId);
    toggleFileExplorer({ type: 'mesh', meshId: propertiesPanelMeshId, path: mesh.path });
  };

  if (propertiesPanelMeshId == null || !mesh) return null;

  return (
    <>
      <div
        className="fixed top-0 right-0 h-full w-[320px] bg-[#0d0d16] border-l border-[#2a2a2a] z-40 flex flex-col shadow-2xl"
        style={{ transform: 'translateX(0)', transition: 'transform 200ms' }}
      >
        {/* Header */}
        <div className="px-4 py-3 border-b border-[#2a2a2a] flex items-center justify-between shrink-0">
          <div className="min-w-0 flex-1">
            <h2 className="text-sm font-semibold text-[#e0e0e0]">Mesh Properties</h2>
            <p className="text-xs text-[#6b7280] truncate">{mesh.name}</p>
          </div>
          <button
            onClick={closePropertiesPanel}
            className="text-[#6b7280] hover:text-[#e0e0e0] transition-colors shrink-0"
            title="Close"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        {/* Form */}
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {loading ? (
            <div className="text-center text-[#6b7280] text-xs py-8">Loading...</div>
          ) : (
            <>
              {/* Name */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-1">Name</label>
                <input
                  type="text"
                  value={form.name}
                  onChange={(e) => setForm((p) => ({ ...p, name: e.target.value }))}
                  onBlur={async (e) => {
                    if (!mountedRef.current) return;
                    await saveName(e.target.value);
                    git?.refresh();
                  }}
                  className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1.5 text-sm text-[#e0e0e0] focus:outline-none focus:border-[#00d4ff]"
                />
              </div>

              {/* Directory (read-only) */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-1">Directory</label>
                <input
                  type="text"
                  value={mesh.path}
                  readOnly
                  className="w-full bg-[#111119] border border-[#2a2a2a] rounded px-2 py-1.5 text-xs text-[#6b7280]"
                />
              </div>

              {/* Uncommitted Changes */}
              {git && git.files.length > 0 && (
                <UncommittedChangesSection
                  meshPath={mesh.path}
                  meshName={mesh.name}
                  files={git.files}
                  isAuthenticated={git.isAuthenticated}
                  defaultBranch={git.defaultBranch}
                  onViewDiff={handleViewDiff}
                  onRefresh={() => git.refresh()}
                />
              )}

              {/* AI context portability */}
              <AiContextSection
                meshId={propertiesPanelMeshId}
                meshPath={mesh.path}
                isAuthenticated={git?.isAuthenticated ?? false}
              />

              {/* Model */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-1">
                  Model <span className="text-[#4b5563]">(cwrap only)</span>
                </label>
                <input
                  type="text"
                  value={form.model}
                  onChange={(e) => setForm((p) => ({ ...p, model: e.target.value }))}
                  onBlur={async (e) => {
                    if (!mountedRef.current) return;
                    await saveProperty('model', e.target.value);
                    git?.refresh();
                  }}
                  placeholder="e.g., opus-4, sonnet-4"
                  className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1.5 text-sm text-[#e0e0e0] focus:outline-none focus:border-[#00d4ff]"
                />
              </div>

              {/* Effort */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-1">
                  Effort <span className="text-[#4b5563]">(cwrap only)</span>
                </label>
                <select
                  value={form.effort}
                  onChange={async (e) => {
                    setForm((p) => ({ ...p, effort: e.target.value }));
                    await saveProperty('effort', e.target.value);
                    git?.refresh();
                  }}
                  className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1.5 text-sm text-[#e0e0e0] focus:outline-none focus:border-[#00d4ff]"
                >
                  {EFFORT_OPTIONS.map((o) => (
                    <option key={o.value} value={o.value}>{o.label}</option>
                  ))}
                </select>
              </div>

              {/* Default provider */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-1">Default provider</label>
                <select
                  value={form.defaultProvider}
                  onChange={async (e) => {
                    setForm((p) => ({ ...p, defaultProvider: e.target.value }));
                    await saveProperty('defaultProvider', e.target.value);
                  }}
                  className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1.5 text-sm text-[#e0e0e0] focus:outline-none focus:border-[#00d4ff]"
                >
                  <option value="">&lt;Default&gt; (Anthropic)</option>
                  {providers.map((p) => (
                    <option key={p.id} value={p.id}>{p.label}</option>
                  ))}
                </select>
              </div>

              {/* Project preset */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-1">Project preset</label>
                <select
                  value=""
                  onChange={(e) => {
                    if (e.target.value) void applyPresetById(e.target.value);
                  }}
                  className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1.5 text-sm text-[#e0e0e0] focus:outline-none focus:border-[#00d4ff]"
                >
                  <option value="">Choose a preset to fill Build/Run…</option>
                  {PROJECT_PRESETS.map((p) => (
                    <option key={p.id} value={p.id}>
                      {detected?.preset_id === p.id ? `✓ ${p.label} (detected)` : p.label}
                    </option>
                  ))}
                </select>
                {detected?.preset_id &&
                 !form.buildCommand.trim() &&
                 !form.runCommand.trim() && (
                  <div className="mt-2 flex items-start gap-2 bg-[#0a1a24] border border-[#00d4ff]/30 rounded px-2 py-1.5">
                    <span className="text-xs text-[#9ca3af] flex-1">
                      Looks like a <span className="text-[#e0e0e0]">{detected.label}</span> project.
                    </span>
                    <button
                      type="button"
                      onClick={() => void applyPresetById(detected.preset_id!)}
                      className="text-xs text-[#00d4ff] hover:text-[#7fe5ff] font-medium"
                    >
                      Apply preset
                    </button>
                  </div>
                )}
              </div>

              {/* Use Worktree */}
              <div>
                <label className="flex items-center gap-2 text-xs text-[#e0e0e0] cursor-pointer">
                  <input
                    type="checkbox"
                    checked={form.useWorktree}
                    onChange={async (e) => {
                      setForm((p) => ({ ...p, useWorktree: e.target.checked }));
                      await saveProperty('useWorktree', e.target.checked);
                      git?.refresh();
                    }}
                    className="accent-[#00d4ff]"
                  />
                  <span>Use worktree</span>
                </label>
                {form.useWorktree && (
                  <div className="mt-2 pl-4 space-y-3 border-l border-[#2a2a2a]">
                    {/* Starting point */}
                    <div>
                      <label className="block text-xs text-[#9ca3af] mb-2">Starting point</label>
                      <div className="space-y-2">
                        {BASEREF_OPTIONS.map((o) => (
                          <label key={o.value} className="flex items-start gap-2 text-xs text-[#e0e0e0] cursor-pointer">
                            <input
                              type="radio"
                              name="baseRef"
                              value={o.value}
                              checked={form.baseRef === o.value}
                              onChange={async (e) => {
                                setForm((p) => ({ ...p, baseRef: e.target.value }));
                                await saveProperty('baseRef', e.target.value);
                                git?.refresh();
                              }}
                              className="mt-0.5 accent-[#00d4ff]"
                            />
                            <span>{o.label}</span>
                          </label>
                        ))}
                      </div>
                    </div>
                    {/* Worktree mode */}
                    <div>
                      <label className="block text-xs text-[#9ca3af] mb-2">Worktree mode</label>
                      <div className="space-y-2">
                        {WORKTREE_MODE_OPTIONS.map((o) => (
                          <label key={o.value} className="flex items-start gap-2 text-xs text-[#e0e0e0] cursor-pointer">
                            <input
                              type="radio"
                              name="worktreeMode"
                              value={o.value}
                              checked={form.worktreeMode === o.value}
                              onChange={async (e) => {
                                setForm((p) => ({ ...p, worktreeMode: e.target.value }));
                                await saveProperty('worktreeMode', e.target.value);
                                git?.refresh();
                              }}
                              className="mt-0.5 accent-[#00d4ff]"
                            />
                            <span>{o.label}</span>
                          </label>
                        ))}
                      </div>
                    </div>
                  </div>
                )}
              </div>

              {/* Build command */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-1">Build command</label>
                <input
                  type="text"
                  value={form.buildCommand}
                  onChange={(e) => setForm((p) => ({ ...p, buildCommand: e.target.value }))}
                  onBlur={async (e) => {
                    if (!mountedRef.current) return;
                    await saveProperty('buildCommand', e.target.value);
                    git?.refresh();
                  }}
                  placeholder="npm run build"
                  className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1.5 text-sm text-[#e0e0e0] focus:outline-none focus:border-[#00d4ff]"
                />
              </div>

              {/* Run command */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-1">Run command</label>
                <input
                  type="text"
                  value={form.runCommand}
                  onChange={(e) => setForm((p) => ({ ...p, runCommand: e.target.value }))}
                  onBlur={async (e) => {
                    if (!mountedRef.current) return;
                    await saveProperty('runCommand', e.target.value);
                    git?.refresh();
                  }}
                  placeholder="npm run dev"
                  className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1.5 text-sm text-[#e0e0e0] focus:outline-none focus:border-[#00d4ff]"
                />
              </div>

              {/* Branches & Worktrees — extracted to 🌳 Worktree Manager tab (issue #377). */}
            </>
          )}
        </div>

        {/* Footer */}
        <div className="px-4 py-3 border-t border-[#2a2a2a] shrink-0">
          <button
            onClick={() => setShowDeleteConfirm(true)}
            className="w-full bg-[#ef4444]/10 hover:bg-[#ef4444]/20 text-[#ef4444] text-xs font-medium py-2 rounded transition-colors"
          >
            Delete Mesh
          </button>
        </div>
      </div>

      {showDeleteConfirm && (
        <ConfirmDialog
          title="Delete Mesh"
          message={`Delete "${mesh.name}" and all its agent nodes? This cannot be undone.`}
          confirmLabel="Delete"
          onConfirm={handleDelete}
          onCancel={() => setShowDeleteConfirm(false)}
        />
      )}
    </>
  );
}
