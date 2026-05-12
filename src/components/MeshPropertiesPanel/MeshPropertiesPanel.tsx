import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';

interface MeshConfig {
  build_command: string | null;
  run_command: string | null;
  model: string | null;
  effort: string | null;
  base_ref: string | null;
  in_place: boolean;
}

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
  { value: 'in-place', label: 'In-place — work directly in repo (no worktree)' },
];

export function MeshPropertiesPanel() {
  const propertiesPanelMeshId = useUIStore((s) => s.propertiesPanelMeshId);
  const closePropertiesPanel = useUIStore((s) => s.closePropertiesPanel);
  const mesh = useMeshStore((s) =>
    propertiesPanelMeshId != null ? s.meshesById.get(propertiesPanelMeshId) : undefined
  );
  const updateMeshName = useMeshStore((s) => s.updateMeshName);
  const deleteMesh = useMeshStore((s) => s.deleteMesh);

  const [initialConfig, setInitialConfig] = useState<MeshConfig | null>(null);
  const [form, setForm] = useState({
    name: '',
    model: '',
    effort: '',
    baseRef: 'fresh',
    buildCommand: '',
    runCommand: '',
    inPlace: false,
  });
  const [saving, setSaving] = useState(false);
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  const [loading, setLoading] = useState(true);

  // Load config on mount
  useEffect(() => {
    if (propertiesPanelMeshId == null) return;
    setLoading(true);
    invoke<MeshConfig>('get_mesh_properties', { meshId: propertiesPanelMeshId })
      .then((config) => {
        setInitialConfig(config);
        setForm({
          name: mesh?.name ?? '',
          model: config.model ?? '',
          effort: config.effort ?? '',
          baseRef: config.in_place ? 'in-place' : (config.base_ref === 'HEAD' ? 'head' : config.base_ref?.includes('origin') ? 'fresh' : 'fresh'),
          buildCommand: config.build_command ?? '',
          runCommand: config.run_command ?? '',
          inPlace: config.in_place ?? false,
        });
        setLoading(false);
      })
      .catch(() => setLoading(false));
  }, [propertiesPanelMeshId]);

  if (propertiesPanelMeshId == null || !mesh) return null;

  const handleSave = async () => {
    setSaving(true);
    try {
      if (form.name !== mesh.name) {
        await updateMeshName(propertiesPanelMeshId, form.name);
      }

      if (form.model !== (initialConfig?.model ?? '')) {
        if (form.model) {
          await invoke('update_mesh_field', { meshId: propertiesPanelMeshId, section: 'agent', key: 'model', value: form.model });
        } else {
          await invoke('update_mesh_field', { meshId: propertiesPanelMeshId, section: 'agent', key: 'model', value: '' });
        }
      }

      if (form.effort) {
        await invoke('update_mesh_field', { meshId: propertiesPanelMeshId, section: 'agent', key: 'effort', value: form.effort });
      }

      // Handle baseRef and in_place: in-place is a third option, not part of baseRef
      if (form.baseRef === 'in-place') {
        await invoke('update_mesh_in_place', { meshId: propertiesPanelMeshId, inPlace: true });
      } else {
        // Not in-place: save in_place=false and baseRef as usual
        await invoke('update_mesh_in_place', { meshId: propertiesPanelMeshId, inPlace: false });
        if (form.baseRef === 'fresh') {
          await invoke('update_worktree_base_ref', { meshId: propertiesPanelMeshId, baseRef: 'fresh' });
        } else {
          await invoke('update_worktree_base_ref', { meshId: propertiesPanelMeshId, baseRef: 'head' });
        }
      }

      if (form.buildCommand !== (initialConfig?.build_command ?? '')) {
        await invoke('update_mesh_field', { meshId: propertiesPanelMeshId, section: 'build', key: 'command', value: form.buildCommand });
      }

      if (form.runCommand !== (initialConfig?.run_command ?? '')) {
        await invoke('update_mesh_field', { meshId: propertiesPanelMeshId, section: 'run', key: 'command', value: form.runCommand });
      }

      closePropertiesPanel();
    } catch (e) {
      console.error('Failed to save mesh properties:', e);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    try {
      await deleteMesh(propertiesPanelMeshId);
      closePropertiesPanel();
    } catch (e) {
      console.error('Failed to delete mesh:', e);
    }
  };

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
            className="text-[#6b7280] hover:text-[#e0e0e0] text-lg leading-none ml-2 shrink-0"
          >
            ×
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

              {/* Model */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-1">
                  Model <span className="text-[#4b5563]">(cwrap only)</span>
                </label>
                <input
                  type="text"
                  value={form.model}
                  onChange={(e) => setForm((p) => ({ ...p, model: e.target.value }))}
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
                  onChange={(e) => setForm((p) => ({ ...p, effort: e.target.value }))}
                  className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1.5 text-sm text-[#e0e0e0] focus:outline-none focus:border-[#00d4ff]"
                >
                  {EFFORT_OPTIONS.map((o) => (
                    <option key={o.value} value={o.value}>{o.label}</option>
                  ))}
                </select>
              </div>

              {/* baseRef */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-2">baseRef</label>
                <div className="space-y-2">
                  {BASEREF_OPTIONS.map((o) => (
                    <label key={o.value} className="flex items-start gap-2 text-xs text-[#e0e0e0] cursor-pointer">
                      <input
                        type="radio"
                        name="baseRef"
                        value={o.value}
                        checked={form.baseRef === o.value}
                        onChange={(e) => setForm((p) => ({ ...p, baseRef: e.target.value }))}
                        className="mt-0.5 accent-[#00d4ff]"
                      />
                      <span>{o.label}</span>
                    </label>
                  ))}
                </div>
              </div>

              {/* Build command */}
              <div>
                <label className="block text-xs text-[#9ca3af] mb-1">Build command</label>
                <input
                  type="text"
                  value={form.buildCommand}
                  onChange={(e) => setForm((p) => ({ ...p, buildCommand: e.target.value }))}
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
                  placeholder="npm run dev"
                  className="w-full bg-[#1a1a2e] border border-[#2a2a2a] rounded px-2 py-1.5 text-sm text-[#e0e0e0] focus:outline-none focus:border-[#00d4ff]"
                />
              </div>
            </>
          )}
        </div>

        {/* Footer */}
        <div className="px-4 py-3 border-t border-[#2a2a2a] space-y-2 shrink-0">
          <button
            onClick={handleSave}
            disabled={saving || loading}
            className="w-full bg-[#00d4ff]/10 hover:bg-[#00d4ff]/20 text-[#00d4ff] text-xs font-medium py-2 rounded transition-colors disabled:opacity-50"
          >
            {saving ? 'Saving...' : 'Save & Close'}
          </button>
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
