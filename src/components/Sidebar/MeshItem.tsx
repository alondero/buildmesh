import { useState, useEffect, useRef } from 'react';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type { Mesh } from '../../stores/meshStore';
import type { AgentNode } from '../../stores/agentNodeStore';
import { getMeshColor } from '../../lib/meshColors';
import { gitSync } from '../../lib/tauri';
import type { MeshHealth } from '../../lib/tauri';
import { useGitBranchStatus } from '../../hooks/useGitBranchStatus';
import { useMeshHealth } from '../../hooks/useMeshHealth';
import { NodeItem } from './NodeItem';
import { NodeCreationForm } from './NodeCreationForm';
import type { ProviderEntry } from './ProviderDropdown';

/// Build the tooltip text for the sidebar drift `!` badge. Lists the
/// reasons in priority order — hostage first (it blocks a restore), then
/// drift, then dirty / unpushed. Mirrors the issue spec's "what to fix
/// first" priority.
function buildDriftTooltip(health: MeshHealth): string {
  const lines: string[] = [];
  if (health.base_branch_holder) {
    const h = health.base_branch_holder;
    const localBase = health.local_base_branch ?? 'main';
    lines.push(`${localBase} held by ${h.name} — click to fix`);
  }
  if (health.is_drifted) {
    const localBase = health.local_base_branch ?? 'base';
    const current = health.current_branch ?? `detached @ ${health.current_short_sha}`;
    lines.push(`Root on ${current}, base is ${localBase}`);
  }
  if (health.is_dirty) lines.push('uncommitted changes');
  if (health.unpushed_ahead > 0) {
    lines.push(`${health.unpushed_ahead} unpushed commit${health.unpushed_ahead === 1 ? '' : 's'}`);
  }
  return lines.join('\n');
}

interface MeshItemProps {
  mesh: Mesh;
  isSelected: boolean;
  isDropdownOpen: boolean;
  providerList: ProviderEntry[];
  onSelectMesh: (id: number) => void;
  onNewNode: (mesh: Mesh) => void;
  onSelectProvider: (mesh: Mesh, providerId: string, useWorktree?: boolean) => void;
  // Issue #376: opens the unified Probe Panel on the 📁 (Project Files) tab
  // for this mesh. Replaces the legacy `onToggleFileExplorer` prop, which
  // toggled the deleted SessionView left-pane `FileExplorerPanel`.
  onOpenFilesProbe: () => void;
  meshNodes: AgentNode[];
  activeNodeId: number | null;
  setActiveNode: (id: number) => void;
  selectMesh: (id: number | null) => void;
  onDeleteNode: (e: React.MouseEvent, nodeId: number) => void;
  // Issue #378: opens the Probe Panel on the 🐙 Git Issues tab for this
  // mesh. Replaces the legacy `onOpenGitHubIssues` prop, which mounted
  // the deleted `GitHubIssuesModal`.
  onOpenIssuesProbe: (meshId: number) => void;
  // Issue #378: opens the Probe Panel on the 🕒 Session History tab.
  // Replaces the legacy `onOpenSessionBrowser` prop, which mounted the
  // deleted `SessionBrowserModal`.
  onOpenSessionHistoryProbe: (meshId: number) => void;
  getDefaultProvider: (meshId: number) => Promise<string>;
  /**
   * Issue #375 — the right-click "Properties" item and the drift `!` badge
   * both jump straight to the Probe Panel on the ⚙️ Mesh Properties tab.
   * The handler is responsible for selecting the mesh (so `useProbeContext`
   * resolves to the right row) before flipping the probe open.
   */
  onOpenPropertiesProbe: (meshId: number) => void;
}

export function MeshItem({
  mesh,
  isSelected,
  isDropdownOpen,
  providerList,
  onSelectMesh,
  onNewNode,
  onSelectProvider,
  onOpenFilesProbe,
  meshNodes,
  activeNodeId,
  setActiveNode,
  selectMesh,
  onDeleteNode,
  onOpenIssuesProbe,
  onOpenSessionHistoryProbe,
  getDefaultProvider,
  onOpenPropertiesProbe,
}: MeshItemProps) {
  const {
    setNodeRef,
    transform,
    transition,
    isDragging,
    attributes,
    listeners,
  } = useSortable({ id: mesh.id });

  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const meshColor = getMeshColor(mesh.id);
  const [syncing, setSyncing] = useState(false);
  const [syncMessage, setSyncMessage] = useState<string | null>(null);
  const syncTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const { branchStatus, refresh: refreshBranchStatus } = useGitBranchStatus(mesh.path);
  const { health } = useMeshHealth(mesh.id, mesh.path);
  const behind = branchStatus?.behind ?? 0;

  const handleSync = async () => {
    setSyncing(true);
    setSyncMessage(null);
    if (syncTimeoutRef.current) clearTimeout(syncTimeoutRef.current);
    try {
      const result = await gitSync(mesh.path);
      setSyncMessage(result.message);
      // The pull may have advanced HEAD — recompute the behind count.
      refreshBranchStatus();
    } catch (e) {
      setSyncMessage(`Sync error: ${e}`);
    } finally {
      setSyncing(false);
      syncTimeoutRef.current = setTimeout(() => setSyncMessage(null), 4000);
    }
  };

  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

  useEffect(() => {
    if (!contextMenu) return;
    const handleClick = () => setContextMenu(null);
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setContextMenu(null);
    };
    document.addEventListener('mousedown', handleClick);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handleClick);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [contextMenu]);

  return (
    <div ref={setNodeRef} style={style} className="mb-1 group/mesh">
      {/* Mesh header — double height with color accent */}
      <div
        className={`border-l-3 rounded-r-md px-2 py-2.5 cursor-pointer transition-colors ${meshColor.border} ${
          isSelected ? 'bg-bg-card' : 'hover:bg-bg-card/50'
        }`}
        onClick={() => onSelectMesh(mesh.id)}
        onContextMenu={(e) => {
          e.preventDefault();
          setContextMenu({ x: e.clientX, y: e.clientY });
        }}
      >
        <div className="flex items-center gap-2">
          <span
            {...attributes}
            {...listeners}
            className="text-text-muted hover:text-text-secondary cursor-grab active:cursor-grabbing text-[10px] select-none"
            title="Drag to reorder"
          >
            ⋮⋮
          </span>
          <span
            className="font-sans font-semibold text-[15px] text-text-primary truncate flex-1"
          >
            {mesh.name}
          </span>
          {health && (health.is_drifted || health.base_branch_holder !== null) && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                onOpenPropertiesProbe(mesh.id);
              }}
              title={buildDriftTooltip(health)}
              className="text-[11px] font-bold text-status-warning bg-status-warning-bg/15 hover:bg-status-warning-bg/30 rounded px-1.5 leading-[18px] transition-colors"
              aria-label="Mesh health issue"
            >
              !
            </button>
          )}
          {behind > 0 && (
            <span
              className="text-[11px] font-semibold text-status-warning leading-none tabular-nums"
              title={`${behind} commit${behind === 1 ? '' : 's'} behind upstream`}
            >
              ↓{behind}
            </span>
          )}
          <button
            type="button"
            onClick={(e) => { e.stopPropagation(); handleSync(); }}
            disabled={syncing}
            title={syncing ? 'Syncing…' : 'Sync from upstream'}
            className="text-text-muted hover:text-text-secondary disabled:opacity-50 transition-colors"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className={syncing ? 'animate-spin' : ''}>
              <polyline points="23 4 23 10 17 10"/>
              <polyline points="1 20 1 14 7 14"/>
              <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10"/>
              <path d="M20.49 15a9 9 0 0 1-14.85 3.36L1 14"/>
            </svg>
          </button>
          <NodeCreationForm
            mesh={mesh}
            isDropdownOpen={isDropdownOpen}
            providers={providerList}
            onToggleDropdown={onNewNode}
            onSelectProvider={onSelectProvider}
            getDefaultProvider={getDefaultProvider}
          />
        </div>
      </div>
      {syncMessage && (
        <div className="ml-2 mr-2 mb-1 px-2 py-1 rounded text-xs bg-bg-overlay border border-border-subtle text-text-secondary">
          {syncMessage}
        </div>
      )}

      {/* Agent nodes within this mesh */}
      {meshNodes.map(node => (
        <NodeItem
          key={node.id}
          node={node}
          meshColor={meshColor}
          isActive={activeNodeId === node.id}
          onSelect={() => {
            setActiveNode(node.id);
            selectMesh(node.mesh_id);
          }}
          onDelete={(e) => onDeleteNode(e, node.id)}
        />
      ))}

      {/* Context menu — periphery actions */}
      {contextMenu && (
        <div
          className="fixed bg-bg-overlay border border-border-default rounded shadow-lg z-[100] py-1 min-w-[180px]"
          style={{ top: contextMenu.y, left: contextMenu.x }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <button
            onClick={() => { setContextMenu(null); onOpenPropertiesProbe(mesh.id); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3"/>
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>
            </svg>
            Properties
          </button>
          <button
            onClick={() => { setContextMenu(null); onOpenFilesProbe(); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>
            </svg>
            File Explorer
          </button>
          <button
            onClick={() => { setContextMenu(null); handleSync(); }}
            disabled={syncing}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2 disabled:opacity-50"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className={syncing ? 'animate-spin' : ''}>
              <polyline points="23 4 23 10 17 10"/>
              <polyline points="1 20 1 14 7 14"/>
              <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10"/>
              <path d="M20.49 15a9 9 0 0 1-14.85 3.36L1 14"/>
            </svg>
            {syncing ? 'Syncing...' : 'Sync Latest'}
          </button>
          <button
            onClick={() => { setContextMenu(null); onOpenSessionHistoryProbe(mesh.id); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="10"/>
              <polyline points="12 6 12 12 16 14"/>
            </svg>
            Previous Sessions
          </button>
          <button
            onClick={() => { setContextMenu(null); onOpenIssuesProbe(mesh.id); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="10"/>
              <line x1="12" y1="8" x2="12" y2="16"/>
              <line x1="8" y1="12" x2="16" y2="12"/>
            </svg>
            GitHub Issues
          </button>
        </div>
      )}
    </div>
  );
}
