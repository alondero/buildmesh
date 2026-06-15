import { useState, useEffect } from 'react';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useUIStore } from '../../stores/uiStore';
import type { Mesh } from '../../stores/meshStore';
import { listProviders } from '../../lib/tauri';
import Wordmark from '../../assets/wordmark.png';
import { RemoteAccessModal } from '../RemoteAccess/RemoteAccessModal';
import { GitHubIssuesModal } from '../GitHubIssues/GitHubIssuesModal';
import { SessionBrowserModal } from '../SessionBrowser/SessionBrowserModal';
import { AppSettingsModal } from '../AppSettings/AppSettingsModal';
import { DndContext, type DragEndEvent } from '@dnd-kit/core';
import { SortableContext, verticalListSortingStrategy } from '@dnd-kit/sortable';
import { MeshItem } from './MeshItem';
import { colorClassForProvider, type ProviderEntry } from './ProviderDropdown';
import { useSidebarResize } from './useSidebarResize';

export function Sidebar() {
  const { width, isResizing, handleMouseDown } = useSidebarResize();
  const [providerData, setProviderData] = useState<ProviderEntry[]>([]);

  // Fetch available providers from the backend. Platform filtering (e.g. macOS-only
  // Anthropic) is decided server-side via AgentProvider::available_on().
  useEffect(() => {
    listProviders()
      .then(backendProviders => setProviderData(
        backendProviders.map(p => ({ id: p.id, label: p.label, color: colorClassForProvider(p.id) })),
      ))
      .catch(err => console.error('listProviders failed:', err));
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
  const selectProviderForMesh = useAgentNodeStore(state => state.selectProviderForMesh);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);
  const openPropertiesPanel = useUIStore(s => s.openPropertiesPanel);
  const toggleFileExplorer = useUIStore(s => s.toggleFileExplorer);

  const [openDropdownFor, setOpenDropdownFor] = useState<number | null>(null);
  const [remoteAccessOpen, setRemoteAccessOpen] = useState(false);
  const [appSettingsOpen, setAppSettingsOpen] = useState(false);
  const [githubIssuesModal, setGithubIssuesModal] = useState<{ meshId: number; meshPath: string } | null>(null);
  const [sessionBrowserModal, setSessionBrowserModal] = useState<{ meshId: number; meshPath: string } | null>(null);

  // Close the provider dropdown when clicking outside of it.
  useEffect(() => {
    function handleClickOutside(e: MouseEvent) {
      const target = e.target as HTMLElement;
      if (openDropdownFor !== null && !target.closest('[data-dropdown-for]')) setOpenDropdownFor(null);
    }
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [openDropdownFor]);

  const handleSelectMesh = (meshId: number) => selectMesh(selectedMeshId === meshId ? null : meshId);
  const handleToggleDropdown = (mesh: Mesh) => setOpenDropdownFor(openDropdownFor === mesh.id ? null : mesh.id);

  const handleSelectProvider = async (mesh: Mesh, providerId: string, useWorktree?: boolean) => {
    setOpenDropdownFor(null);
    // The create→activate→select-mesh dance + its rollback contract live in
    // selectProviderForMesh (issue #283); this handler stays a thin UI shim.
    try {
      await selectProviderForMesh(mesh.id, mesh.name, mesh.path, providerId, useWorktree);
    } catch (e) {
      console.error('Failed to create node:', e);
    }
  };

  const openModalForMesh = (meshId: number, set: (v: { meshId: number; meshPath: string }) => void) => {
    const mesh = meshes.find(m => m.id === meshId);
    if (mesh) set({ meshId, meshPath: mesh.path });
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
    <div className="relative flex h-full" style={{ width }}>
      <div
        onMouseDown={handleMouseDown}
        className={`absolute top-0 right-0 w-1 h-full cursor-col-resize hover:bg-accent-cyan/30 ${isResizing ? 'bg-accent-cyan/50' : 'bg-transparent'} transition-colors z-10`}
      />

      <div className="w-full bg-bg-surface border-r border-border-subtle flex flex-col h-full overflow-hidden">
        {/* Header */}
        <div className="px-3 pb-2 pt-1.5 border-b border-border-subtle flex items-center gap-2">
          <img src={Wordmark} className="h-8 w-auto max-w-full" alt="Buildmesh" />
          <button
            onClick={() => setAppSettingsOpen(true)}
            className="ml-auto text-text-muted hover:text-accent-cyan transition-colors"
            title="Settings"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3"/>
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>
            </svg>
          </button>
          <button
            onClick={() => setRemoteAccessOpen(true)}
            className="text-accent-cyan hover:text-accent-blue transition-colors"
            title="Remote access"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <rect x="5" y="2" width="14" height="20" rx="2" ry="2"/>
              <line x1="12" y1="18" x2="12" y2="18"/>
            </svg>
          </button>
        </div>

        {appSettingsOpen && <AppSettingsModal onClose={() => setAppSettingsOpen(false)} />}
        {remoteAccessOpen && <RemoteAccessModal onClose={() => setRemoteAccessOpen(false)} />}
        {githubIssuesModal && (
          <GitHubIssuesModal
            meshId={githubIssuesModal.meshId}
            meshPath={githubIssuesModal.meshPath}
            providerList={providerData}
            getDefaultProvider={getDefaultProvider}
            onClose={() => setGithubIssuesModal(null)}
          />
        )}
        {sessionBrowserModal && (
          <SessionBrowserModal meshId={sessionBrowserModal.meshId} meshPath={sessionBrowserModal.meshPath} providerList={providerData} onClose={() => setSessionBrowserModal(null)} />
        )}

        {/* Meshes list */}
        <div className="flex-1 overflow-y-auto">
          <div className="p-2">
            {meshes.length === 0 ? (
              <p className="text-[11px] text-text-muted px-2 py-4 text-center font-sans">
                No meshes yet. Click + New mesh to get started.
              </p>
            ) : (
              <DndContext onDragEnd={handleDragEnd}>
                <SortableContext items={meshes.map(p => p.id)} strategy={verticalListSortingStrategy}>
                  {meshes.map(mesh => (
                    <MeshItem
                      key={mesh.id}
                      mesh={mesh}
                      isSelected={selectedMeshId === mesh.id}
                      isDropdownOpen={openDropdownFor === mesh.id}
                      providerList={providerData}
                      onSelectMesh={handleSelectMesh}
                      onNewNode={handleToggleDropdown}
                      onSelectProvider={handleSelectProvider}
                      onOpenProperties={openPropertiesPanel}
                      onToggleFileExplorer={toggleFileExplorer}
                      meshNodes={agentNodes.filter(w => w.mesh_id === mesh.id)}
                      activeNodeId={activeNodeId}
                      setActiveNode={setActiveNode}
                      selectMesh={selectMesh}
                      onDeleteNode={handleDeleteNode}
                      onOpenGitHubIssues={id => openModalForMesh(id, setGithubIssuesModal)}
                      onOpenSessionBrowser={id => openModalForMesh(id, setSessionBrowserModal)}
                      getDefaultProvider={getDefaultProvider}
                    />
                  ))}
                </SortableContext>
              </DndContext>
            )}
          </div>
        </div>

        {/* Add mesh */}
        <button
          onClick={() => addMesh()}
          className="w-full px-3 py-2.5 flex items-center justify-center gap-1.5 text-[12px] font-sans text-accent-cyan hover:text-accent-blue border-t border-dashed border-border-subtle hover:bg-bg-card/40 transition-colors"
        >
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
            <line x1="12" y1="5" x2="12" y2="19"/>
            <line x1="5" y1="12" x2="19" y2="12"/>
          </svg>
          New mesh
        </button>
      </div>
    </div>
  );
}
