import { useState, useEffect, useRef } from 'react';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useUIStore } from '../../stores/uiStore';
import type { Mesh } from '../../stores/meshStore';
import type { AgentNode } from '../../stores/agentNodeStore';
import type { FileExplorerContext } from '../../stores/uiStore';
import { getStatusConfig } from '../../lib/status';
import { listProviders } from '../../lib/tauri';
import Wordmark from '../../assets/wordmark.png';
import { RemoteAccessModal } from '../RemoteAccess/RemoteAccessModal';
import { GitHubIssuesModal } from '../GitHubIssues/GitHubIssuesModal';
import { SessionBrowserModal } from '../SessionBrowser/SessionBrowserModal';
import {
  DndContext,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  useSortable,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';

// Tailwind class map for provider badges. Stays in the frontend because Tailwind's
// purge tool needs static class strings — backend can't emit these dynamically.
// Backend ProviderInfo.color (hex) is not used here for the same reason.
function colorClassForProvider(providerId: string): string {
  const map: Record<string, string> = {
    anthropic: 'bg-blue-500',
    minimax: 'bg-indigo-500',
    gemini: 'bg-emerald-500',
    opencode: 'bg-amber-500',
  };
  return map[providerId] ?? 'bg-gray-500';
}

type ProviderEntry = { id: string; label: string; color: string };

export function Sidebar() {
  const [sidebarWidth, setSidebarWidth] = useState(256);
  const [isResizing, setIsResizing] = useState(false);
  const [providerData, setProviderData] = useState<ProviderEntry[]>([]);
  const resizingRef = useRef(false);
  const startXRef = useRef(0);
  const startWidthRef = useRef(256);

  const handleMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    resizingRef.current = true;
    startXRef.current = e.clientX;
    startWidthRef.current = sidebarWidth;
    setIsResizing(true);
  };

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!resizingRef.current) return;
      const delta = e.clientX - startXRef.current;
      const newWidth = Math.max(192, Math.min(480, startWidthRef.current + delta));
      setSidebarWidth(newWidth);
    };

    const handleMouseUp = () => {
      if (resizingRef.current) {
        resizingRef.current = false;
        setIsResizing(false);
      }
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    return () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
    };
  }, [sidebarWidth]);

  // Fetch available providers from the backend. Platform filtering (e.g. macOS-only
  // Anthropic) is decided server-side via AgentProvider::available_on().
  useEffect(() => {
    listProviders().then(backendProviders => {
      const mapped = backendProviders.map(p => ({
        id: p.id,
        label: p.label,
        color: colorClassForProvider(p.id),
      }));
      setProviderData(mapped);
    }).catch(err => {
      console.error('listProviders failed:', err);
    });
  }, []);

  const meshes = useMeshStore(state => state.meshes);
  const addMesh = useMeshStore(state => state.addMesh);
  const selectedMeshId = useMeshStore(state => state.selectedMeshId);
  const selectMesh = useMeshStore(state => state.selectMesh);
  const reorderMeshes = useMeshStore(state => state.reorderMeshes);
  const getDefaultProvider = useMeshStore(state => state.getDefaultProvider);
  const agentNodes = useAgentNodeStore(state => state.agentNodes);
  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);
  const createAgentNode = useAgentNodeStore(state => state.createAgentNode);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);

  const [openDropdownFor, setOpenDropdownFor] = useState<number | null>(null);
  const [remoteAccessMeshId, setRemoteAccessMeshId] = useState<boolean>(false);
  const [githubIssuesModal, setGithubIssuesModal] = useState<{ meshId: number; meshPath: string } | null>(null);
  const [sessionBrowserModal, setSessionBrowserModal] = useState<{ meshId: number; meshPath: string } | null>(null);
  const openPropertiesPanel = useUIStore((s) => s.openPropertiesPanel);
  const toggleFileExplorer = useUIStore((s) => s.toggleFileExplorer);
  const fileExplorerContext = useUIStore(s => s.fileExplorerContext);

  const handleSelectMesh = (meshId: number) => {
    if (selectedMeshId === meshId) {
      selectMesh(null);
    } else {
      selectMesh(meshId);
    }
  };

  // Close dropdown when clicking outside
  useEffect(() => {
    function handleClickOutside(e: MouseEvent) {
      const target = e.target as HTMLElement;
      const clickedInsideDropdown = target.closest('[data-dropdown-for]');
      if (openDropdownFor !== null && !clickedInsideDropdown) {
        setOpenDropdownFor(null);
      }
    }
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [openDropdownFor]);

  const handleAddMesh = async () => {
    await addMesh();
  };

  const handleNewNode = async (mesh: Mesh) => {
    setOpenDropdownFor(openDropdownFor === mesh.id ? null : mesh.id);
  };

  const handleSelectProvider = async (mesh: Mesh, providerId: string) => {
    setOpenDropdownFor(null);
    try {
      const node = await createAgentNode(mesh.id, mesh.name, mesh.path, 'main', providerId);
      await setActiveNode(node.id);
      selectMesh(mesh.id);
    } catch (e) {
      console.error('Failed to create node:', e);
    }
  };

  const handleOpenGitHubIssues = (meshId: number) => {
    const mesh = meshes.find(m => m.id === meshId);
    if (mesh) {
      setGithubIssuesModal({ meshId, meshPath: mesh.path });
    }
  };

  const handleOpenSessionBrowser = (meshId: number) => {
    const mesh = meshes.find(m => m.id === meshId);
    if (mesh) {
      setSessionBrowserModal({ meshId, meshPath: mesh.path });
    }
  };

  const handleDeleteNode = async (e: React.MouseEvent, nodeId: number) => {
    e.stopPropagation();
    await deleteAgentNode(nodeId);
  };

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const activeIndex = meshes.findIndex(p => p.id === active.id);
    const overIndex = meshes.findIndex(p => p.id === over.id);
    if (activeIndex === -1 || overIndex === -1) return;
    reorderMeshes(active.id as number, overIndex);
  };

  return (
    <div className="relative flex h-full" style={{ width: sidebarWidth }}>
      {/* Resize handle */}
      <div
        onMouseDown={handleMouseDown}
        className={`absolute top-0 right-0 w-1 h-full cursor-col-resize hover:bg-accent-cyan/30 ${isResizing ? 'bg-accent-cyan/50' : 'bg-transparent'} transition-colors z-10`}
      />

      <div className="w-full bg-bg-surface border-r border-border-subtle flex flex-col h-full overflow-hidden">
        {/* Header */}
        <div className="px-3 pb-2 pt-1.5 border-b border-border-subtle flex items-center gap-2">
          <img src={Wordmark} className="h-8 w-auto max-w-full" alt="Buildmesh" />
          {/* Remote access button — always visible */}
          <button
            onClick={() => setRemoteAccessMeshId(true)}
            className="ml-auto text-text-muted hover:text-accent-cyan transition-colors"
            title="Remote access"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <rect x="5" y="2" width="14" height="20" rx="2" ry="2"/>
              <line x1="12" y1="18" x2="12" y2="18"/>
            </svg>
          </button>
        </div>

      {/* Remote Access Modal */}
      {remoteAccessMeshId && (
        <RemoteAccessModal
          onClose={() => setRemoteAccessMeshId(false)}
        />
      )}

      {/* GitHub Issues Modal */}
      {githubIssuesModal && (
        <GitHubIssuesModal
          meshId={githubIssuesModal.meshId}
          meshPath={githubIssuesModal.meshPath}
          onClose={() => setGithubIssuesModal(null)}
        />
      )}

      {/* Session Browser Modal */}
      {sessionBrowserModal && (
        <SessionBrowserModal
          meshId={sessionBrowserModal.meshId}
          meshPath={sessionBrowserModal.meshPath}
          providerList={providerData}
          onClose={() => setSessionBrowserModal(null)}
        />
      )}

      {/* Meshes list */}
      <div className="flex-1 overflow-y-auto">
        <div className="p-2">
          <div className="flex items-center justify-between mb-1 px-2">
            <span className="text-xs font-medium text-text-secondary uppercase">Meshes</span>
            <button
              onClick={handleAddMesh}
              className="text-xs text-accent-cyan hover:text-accent-blue transition-colors"
            >
              + Add
            </button>
          </div>

          {meshes.length === 0 ? (
            <p className="text-xs text-text-muted px-2 py-4 text-center">
              No meshes yet.{'\n'}Click + Add to get started.
            </p>
          ) : (
            <DndContext onDragEnd={handleDragEnd}>
              <SortableContext items={meshes.map(p => p.id)} strategy={verticalListSortingStrategy}>
                {meshes.map(mesh => {
                  const meshNodes = agentNodes.filter(w => w.mesh_id === mesh.id);
                  const isDropdownOpen = openDropdownFor === mesh.id;
                  return (
                    <SortableMesh
                      key={mesh.id}
                      mesh={mesh}
                      isSelected={selectedMeshId === mesh.id}
                      isDropdownOpen={isDropdownOpen}
                      providerList={providerData}
                      onSelectMesh={handleSelectMesh}
                      onNewNode={handleNewNode}
                      onSelectProvider={handleSelectProvider}
                      onOpenProperties={openPropertiesPanel}
                      onToggleFileExplorer={toggleFileExplorer}
                      fileExplorerContext={fileExplorerContext}
                      meshNodes={meshNodes}
                      activeNodeId={activeNodeId}
                      setActiveNode={setActiveNode}
                      selectMesh={selectMesh}
                      onDeleteNode={handleDeleteNode}
                      onOpenGitHubIssues={handleOpenGitHubIssues}
                      onOpenSessionBrowser={handleOpenSessionBrowser}
                      getDefaultProvider={getDefaultProvider}
                    />
                  );
                })}
              </SortableContext>
            </DndContext>
          )}
        </div>
      </div>

      {/* Footer */}
      <div className="p-2 border-t border-border-subtle text-xs text-text-muted">
        <span>{agentNodes.filter(w => w.status === 'running').length} active</span>
      </div>
      </div>
    </div>
  );
}

function NodeItem({ node, isActive, onSelect, onDelete }: {
  node: AgentNode;
  isActive: boolean;
  onSelect: () => void;
  onDelete: (e: React.MouseEvent) => void;
}) {
  const config = getStatusConfig(node.status);
  return (
    <div
      data-session-item
      data-session-id={node.id}
      onClick={onSelect}
      className={`
        pl-8 pr-1 py-1 rounded cursor-pointer text-sm mb-0.5 flex items-center gap-2
        ${isActive ? 'bg-bg-overlay border border-accent-cyan/50' : 'hover:bg-bg-card border border-transparent'}
      `}
    >
      <span className={config.color} title={config.label}>{config.dot}</span>
      <span className="flex-1 truncate text-text-secondary">{node.name}</span>
      <button
        onClick={onDelete}
        className="text-text-muted hover:text-status-error text-xs px-1 transition-colors"
        title="Delete node"
      >
        ×
      </button>
    </div>
  );
}

interface SortableMeshProps {
  mesh: Mesh;
  isSelected: boolean;
  isDropdownOpen: boolean;
  providerList: Array<{ id: string; label: string; color: string }>;
  onSelectMesh: (id: number) => void;
  onNewNode: (mesh: Mesh) => void;
  onSelectProvider: (mesh: Mesh, providerId: string) => void;
  onOpenProperties: (meshId: number) => void;
  onToggleFileExplorer: (context: FileExplorerContext) => void;
  fileExplorerContext: FileExplorerContext | null;
  meshNodes: AgentNode[];
  activeNodeId: number | null;
  setActiveNode: (id: number) => void;
  selectMesh: (id: number | null) => void;
  onDeleteNode: (e: React.MouseEvent, nodeId: number) => void;
  onOpenGitHubIssues: (meshId: number) => void;
  onOpenSessionBrowser: (meshId: number) => void;
  getDefaultProvider: (meshId: number) => Promise<string>;
}

function SortableMesh({
  mesh,
  isSelected,
  isDropdownOpen,
  providerList,
  onSelectMesh,
  onNewNode,
  onSelectProvider,
  onOpenProperties,
  onToggleFileExplorer,
  fileExplorerContext,
  meshNodes,
  activeNodeId,
  setActiveNode,
  selectMesh,
  onDeleteNode,
  onOpenGitHubIssues,
  onOpenSessionBrowser,
  getDefaultProvider,
}: SortableMeshProps) {
  const isThisMeshOpen = fileExplorerContext?.type === 'mesh' && fileExplorerContext.meshId === mesh.id;
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: mesh.id });

  const [isArrowHovered, setIsArrowHovered] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

  const handleMainClick = async () => {
    const defaultProvider = await getDefaultProvider(mesh.id);
    onSelectProvider(mesh, defaultProvider);
  };

  // Dismiss context menu on click outside or Escape
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
    <div ref={setNodeRef} style={style} className="mb-2 group/mesh">
      <div className="flex items-center gap-1">
        {/* Drag handle */}
        <button
          {...attributes}
          {...listeners}
          className="text-text-muted hover:text-text-secondary cursor-grab active:cursor-grabbing text-xs px-1"
          title="Drag to reorder"
        >
          ⋮⋮
        </button>

        {/* History button — browse previous sessions */}
        <button
          onClick={() => onOpenSessionBrowser(mesh.id)}
          className="text-xs px-1 text-text-muted hover:text-accent-cyan transition-colors"
          title="Browse previous sessions"
        >
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="10"/>
            <polyline points="12 6 12 12 16 14"/>
          </svg>
        </button>

        {/* Split button: main + for default provider, arrow for dropdown */}
        <div className="relative flex">
          {/* Main + button — spawns with default provider */}
          <button
            onClick={handleMainClick}
            className={`text-xs px-1 ${isDropdownOpen ? 'text-accent-blue' : 'text-accent-cyan hover:text-accent-blue'}`}
            title="New node with default provider"
          >
            +
          </button>
          {/* Arrow button — shows provider dropdown */}
          <button
            onClick={() => onNewNode(mesh)}
            onMouseEnter={() => setIsArrowHovered(true)}
            onMouseLeave={() => setIsArrowHovered(false)}
            className={`text-xs px-0.5 ${isArrowHovered || isDropdownOpen ? 'text-accent-blue' : 'text-text-muted hover:text-accent-cyan'}`}
            title="Choose provider"
          >
            ▾
          </button>
          {isDropdownOpen && (
            <div data-dropdown-for={mesh.id} className="absolute left-0 top-full mt-1 z-50 bg-bg-overlay border border-border-default rounded shadow-lg py-1 min-w-[120px]">
              {providerList.map(p => (
                <button
                  key={p.id}
                  onClick={() => onSelectProvider(mesh, p.id)}
                  className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
                >
                  <span className={`w-2 h-2 rounded-full ${p.color}`} />
                  {p.label}
                </button>
              ))}
            </div>
          )}
        </div>
        <div
          onClick={() => { onSelectMesh(mesh.id); }}
          className={`flex-1 px-2 py-1.5 rounded cursor-pointer text-sm hover:bg-bg-card flex items-center ${
            isSelected ? 'text-accent-cyan font-semibold' : 'text-text-secondary'
          }`}
        >
          <span className="flex-1 truncate" onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setContextMenu({ x: e.clientX, y: e.clientY });
          }}>{mesh.name}</span>
          <button
            onClick={(e) => { e.stopPropagation(); onOpenProperties(mesh.id); }}
            className="opacity-0 group-hover/mesh:opacity-100 text-text-muted hover:text-accent-cyan text-xs px-1 transition-all"
            title="Mesh properties"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3"/>
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>
            </svg>
          </button>
          <button
            onClick={(e) => { e.stopPropagation(); onToggleFileExplorer({ type: 'mesh', meshId: mesh.id, path: mesh.path }); }}
            className={`${isThisMeshOpen ? 'opacity-100 bg-accent-green' : 'opacity-100 bg-accent-amber'} text-white text-xs px-1 transition-all`}
            title="Open file explorer"
          >
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>
            </svg>
          </button>
        </div>
      </div>
      {meshNodes.map(node => (
        <NodeItem
          key={node.id}
          node={node}
          isActive={activeNodeId === node.id}
          onSelect={() => {
            setActiveNode(node.id);
            selectMesh(node.mesh_id);
          }}
          onDelete={(e) => onDeleteNode(e, node.id)}
        />
      ))}
      {/* Context menu */}
      {contextMenu && (
        <div
          className="fixed bg-bg-overlay border border-border-default rounded shadow-lg z-[100] py-1 min-w-[160px]"
          style={{ top: contextMenu.y, left: contextMenu.x }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <button
            onClick={() => {
              setContextMenu(null);
              onOpenSessionBrowser(mesh.id);
            }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="10"/>
              <polyline points="12 6 12 12 16 14"/>
            </svg>
            Previous Sessions
          </button>
          <button
            onClick={() => {
              setContextMenu(null);
              onOpenGitHubIssues(mesh.id);
            }}
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