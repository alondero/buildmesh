import { useState, useEffect, useRef } from 'react';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import type { Mesh } from '../../stores/meshStore';
import type { AgentNode } from '../../stores/agentNodeStore';
import { getStatusConfig } from '../../lib/status';
import { isMac } from '../../lib/platform';
import Wordmark from '../../assets/wordmark.png';
import { RemoteAccessModal } from '../RemoteAccess/RemoteAccessModal';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
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

const ALL_PROVIDERS = [
  { id: 'anthropic', label: 'Anthropic', color: 'bg-blue-500' },
  { id: 'minimax', label: 'Minimax', color: 'bg-indigo-500' },
  { id: 'gemini', label: 'Gemini', color: 'bg-emerald-500' },
  { id: 'opencode', label: 'OpenCode', color: 'bg-amber-500' },
];

const PROVIDERS = isMac
  ? ALL_PROVIDERS.filter(p => p.id === 'anthropic')
  : ALL_PROVIDERS;

export function Sidebar() {
  const [sidebarWidth, setSidebarWidth] = useState(256);
  const [isResizing, setIsResizing] = useState(false);
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

  const meshes = useMeshStore(state => state.meshes);
  const addMesh = useMeshStore(state => state.addMesh);
  const deleteMesh = useMeshStore(state => state.deleteMesh);
  const selectedMeshId = useMeshStore(state => state.selectedMeshId);
  const selectMesh = useMeshStore(state => state.selectMesh);
  const reorderMeshes = useMeshStore(state => state.reorderMeshes);
  const agentNodes = useAgentNodeStore(state => state.agentNodes);
  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);
  const createAgentNode = useAgentNodeStore(state => state.createAgentNode);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);

  const [openDropdownFor, setOpenDropdownFor] = useState<number | null>(null);
  const [remoteAccessMeshId, setRemoteAccessMeshId] = useState<boolean>(false);
  const [meshToDelete, setMeshToDelete] = useState<Mesh | null>(null);

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

  const handleDeleteNode = async (e: React.MouseEvent, nodeId: number) => {
    e.stopPropagation();
    await deleteAgentNode(nodeId);
  };

  const handleDeleteMesh = (e: React.MouseEvent, mesh: Mesh) => {
    e.stopPropagation();
    setMeshToDelete(mesh);
  };

  const confirmDeleteMesh = async () => {
    if (!meshToDelete) return;
    const meshId = meshToDelete.id;
    const meshNodes = agentNodes.filter(n => n.mesh_id === meshId);
    for (const node of meshNodes) {
      await deleteAgentNode(node.id);
    }
    if (selectedMeshId === meshId) {
      selectMesh(null);
    }
    await deleteMesh(meshId);
    setMeshToDelete(null);
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

      {/* Delete Mesh Confirmation */}
      {meshToDelete && (
        <ConfirmDialog
          title="Delete mesh"
          message={`Are you sure you want to delete "${meshToDelete.name}"?${agentNodes.filter(n => n.mesh_id === meshToDelete.id).length > 0 ? ' This will also remove all agent nodes within it.' : ''} This action cannot be undone.`}
          confirmLabel="Delete"
          onConfirm={confirmDeleteMesh}
          onCancel={() => setMeshToDelete(null)}
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
                      onSelectMesh={handleSelectMesh}
                      onNewNode={handleNewNode}
                      onSelectProvider={handleSelectProvider}
                      onDeleteMesh={handleDeleteMesh}
                      meshNodes={meshNodes}
                      activeNodeId={activeNodeId}
                      setActiveNode={setActiveNode}
                      selectMesh={selectMesh}
                      onDeleteNode={handleDeleteNode}
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
  onSelectMesh: (id: number) => void;
  onNewNode: (mesh: Mesh) => void;
  onSelectProvider: (mesh: Mesh, providerId: string) => void;
  onDeleteMesh: (e: React.MouseEvent, mesh: Mesh) => void;
  meshNodes: AgentNode[];
  activeNodeId: number | null;
  setActiveNode: (id: number) => void;
  selectMesh: (id: number | null) => void;
  onDeleteNode: (e: React.MouseEvent, nodeId: number) => void;
}

function SortableMesh({
  mesh,
  isSelected,
  isDropdownOpen,
  onSelectMesh,
  onNewNode,
  onSelectProvider,
  onDeleteMesh,
  meshNodes,
  activeNodeId,
  setActiveNode,
  selectMesh,
  onDeleteNode,
}: SortableMeshProps) {
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: mesh.id });

  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

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

        <div className="relative">
          <button
            onClick={() => onNewNode(mesh)}
            className={`text-xs px-1 ${isDropdownOpen ? 'text-accent-blue' : 'text-accent-cyan hover:text-accent-blue'}`}
            title="New node"
          >
            +
          </button>
          {isDropdownOpen && (
            <div data-dropdown-for={mesh.id} className="absolute left-0 top-full mt-1 z-50 bg-bg-overlay border border-border-default rounded shadow-lg py-1 min-w-[120px]">
              {PROVIDERS.map(p => (
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
          onClick={() => onSelectMesh(mesh.id)}
          className={`flex-1 px-2 py-1.5 rounded cursor-pointer text-sm hover:bg-bg-card flex items-center ${
            isSelected ? 'text-accent-cyan font-semibold' : 'text-text-secondary'
          }`}
        >
          <span className="flex-1 truncate">{mesh.name}</span>
          <button
            onClick={(e) => onDeleteMesh(e, mesh)}
            className="opacity-0 group-hover/mesh:opacity-100 text-text-muted hover:text-status-error text-xs px-1 transition-all"
            title="Delete mesh"
          >
            ×
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
    </div>
  );
}