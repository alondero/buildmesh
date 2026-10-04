// PROTOTYPE — variant L ("bordered compact") of the mesh sidebar.
// Throwaway exploration, not production: one bordered card per mesh, a
// colour-bar button that opens the recolour picker (no separate swatch),
// status dots with NO textual status labels, click-to-expand with the nodes
// enclosed in the same card. Iconography, drag-reorder, and the right-click
// context menu are deliberately out of scope here — they ride along unchanged
// (NodeItem) or return in the final fold-in. Enabled in dev via
// `?sidebar=compact` or `localStorage["bm.sidebar"] === "compact"`.
import { memo, useState, useEffect, useMemo, useRef, useCallback } from 'react';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type { Mesh } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import type { AgentNode } from '../../stores/agentNodeStore';
import { getMeshColor } from '../../lib/meshColors';
import { gitSync } from '../../lib/tauri';
import type { MeshHealth } from '../../lib/tauri';
import { listen } from '@tauri-apps/api/event';
import { pathMatchesGitEvent } from '../../lib/paths';
import type { MeshSyncWarningPayload } from '../../types/generated/MeshSyncWarningPayload';
import { useGitBranchStatus } from '../../hooks/useGitBranchStatus';
import { useMeshHealth } from '../../hooks/useMeshHealth';
import { getNodeStatusConfig, needsAgentAttention } from '../../lib/status';
import { NodeCluster } from './NodeCluster';
import type { NodeActivityCluster } from '../../lib/nodeActivities';
import { NodeCreationForm } from './NodeCreationForm';
import { MeshRecolorModal } from '../Mesh/MeshRecolorModal';
import type { SpawnOption } from '../../lib/groups';

/// Mirrors `buildDriftTooltip` in MeshItem: reasons in fix-first priority.
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

interface CompactMeshItemProps {
  mesh: Mesh;
  isSelected: boolean;
  isDropdownOpen: boolean;
  isSpawning: boolean;
  dimmed?: boolean;
  providerList: SpawnOption[];
  onSelectMesh: (id: number) => void;
  onNewNode: (mesh: Mesh) => void;
  onSelectProvider: (mesh: Mesh, providerId: string, useWorktree?: boolean, configurationId?: string) => void;
  onOpenFilesProbe: () => void;
  nodeClusters: NodeActivityCluster[];
  onActivateNode: (id: number) => void;
  selectMesh: (id: number | null) => void;
  onDeleteNode: (e: React.MouseEvent, nodeId: number) => void;
  onOpenIssuesProbe: (meshId: number) => void;
  onOpenSessionHistoryProbe: (meshId: number) => void;
  getDefaultProvider: (meshId: number) => Promise<string>;
  onOpenPropertiesProbe: (meshId: number) => void;
  onOpenWorktreesProbe: (meshId: number) => void;
}

export const CompactMeshItem = memo(CompactMeshItemView);

function CompactMeshItemView({
  mesh,
  isSelected,
  isDropdownOpen,
  isSpawning,
  dimmed = false,
  providerList,
  onSelectMesh,
  onNewNode,
  onSelectProvider,
  nodeClusters,
  onActivateNode,
  selectMesh,
  onDeleteNode,
  getDefaultProvider,
  onOpenWorktreesProbe,
}: CompactMeshItemProps) {
  // The colour bar IS the reorder handle (click = picker, drag = reorder)
  // through the same dnd-kit context as MeshItem. The 5px pointer-sensor
  // grace in Sidebar lets clicks pass through; the press-position guard in
  // `handleBarClick` swallows the stray click a real drag leaves behind.
  const {
    setNodeRef,
    transform,
    transition,
    isDragging,
    attributes,
    listeners,
  } = useSortable({ id: mesh.id });
  // Click-vs-drag on the bar: the press position decides. A release within
  // 5px of the press is a picker click; anything further travelled means a
  // drag just ran and the trailing click is swallowed. (jsdom + userEvent
  // cannot drive clicks through attached dnd-kit activators — the sensor's
  // native listeners consume the emulated sequence — so the click path is
  // covered by fireEvent-sequence tests here and by a real CDP click in the
  // dev-view steps. Keyboard Enter/Space on the bar starts a drag, matching
  // handle semantics elsewhere; keyboard picker access returns at fold-in.)
  const pressPos = useRef<{ x: number; y: number } | null>(null);

  const handleBarClick = (e: React.MouseEvent) => {
    const start = pressPos.current;
    pressPos.current = null;
    if (start && Math.hypot(e.clientX - start.x, e.clientY - start.y) > 5) return;
    e.stopPropagation();
    setRecolorOpen(true);
  };
  const meshColor = useMemo(() => getMeshColor(mesh.id, mesh.color), [mesh.id, mesh.color]);
  const [recolorOpen, setRecolorOpen] = useState(false);
  const { branchStatus } = useGitBranchStatus(mesh.path);
  const { health } = useMeshHealth(mesh.id, mesh.path);
  const behind = branchStatus?.behind ?? 0;

  // Flat, de-duplicated member list across clusters (a node appears once —
  // as root or member — but dedupe by id so future cluster shapes stay safe).
  const members = useMemo(() => {
    const seen = new Set<number>();
    const out: AgentNode[] = [];
    for (const cluster of nodeClusters) {
      for (const node of cluster.members) {
        if (!seen.has(node.id)) {
          seen.add(node.id);
          out.push(node);
        }
      }
    }
    return out;
  }, [nodeClusters]);

  // Hot meshes (attention or error) start expanded; quiet ones start as one
  // line. Initial-only — afterwards the chevron owns the state.
  const [expanded, setExpanded] = useState(() =>
    members.some((node) => needsAgentAttention(node.status) || node.status === 'error'),
  );

  // Failure-only sync indicator with retry-on-click (same backend event as
  // MeshItem's header icon; the prototype has no context menu to host the
  // "Force sync" entry, so the icon itself retries).
  const [syncFailed, setSyncFailed] = useState(false);
  const [syncing, setSyncing] = useState(false);
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    listen<MeshSyncWarningPayload>('mesh-sync-warning', (event) => {
      if (pathMatchesGitEvent({ path: event.payload.mesh_path }, mesh.path)) {
        setSyncFailed(true);
      }
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [mesh.path]);

  const handleSyncRetry = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (syncing) return;
    setSyncing(true);
    try {
      await gitSync(mesh.path);
      setSyncFailed(false);
    } catch (err) {
      console.error('Sync retry failed:', err);
    } finally {
      setSyncing(false);
    }
  };

  const handleSelectNode = useCallback((nodeId: number, meshId: number) => {
    onActivateNode(nodeId);
    if (useUIStore.getState().viewMode !== 'single') {
      selectMesh(meshId);
    }
  }, [onActivateNode, selectMesh]);

  const dotSummary = useMemo(() => {
    if (members.length === 0) return 'No agents';
    const labels = members.map((node) => getNodeStatusConfig(node).label);
    return `${members.length} agent${members.length === 1 ? '' : 's'}: ${labels.join(', ')}`;
  }, [members]);

  const drifted = !!health && (health.is_drifted || health.base_branch_holder !== null);

  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

  return (
    <div
      ref={setNodeRef}
      style={style}
      data-prototype-mesh={mesh.id}
      className={`mb-1.5 rounded-lg border bg-bg-card/60 ${drifted ? 'border-status-warning/40' : 'border-border-subtle'}`}
    >
      {/* Two-line header. Line 1 is identity + actions; line 2 is status
          (dots + count, no textual labels) and doubles as the expand toggle,
          so no chevron takes up horizontal space. */}
      <div
        role="button"
        tabIndex={0}
        aria-label={`${mesh.name}, ${dotSummary}`}
        onClick={() => onSelectMesh(mesh.id)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onSelectMesh(mesh.id);
          }
        }}
        className={`rounded-lg px-1.5 pt-1.5 pb-1 cursor-pointer transition-colors ${
          isSelected ? 'bg-bg-card' : 'hover:bg-bg-card-hover/60'
        }`}
      >
        <div className="flex items-stretch gap-2">
          {/* The vertical bar is picker AND reorder handle: click opens the
              colour picker, drag reorders the mesh. Hover fattens the bar and
              shows the grab cursor so the handle reads with no extra chrome. */}
          <button
            type="button"
            {...attributes}
            {...listeners}
            onPointerDown={(e) => {
              // Chain the sensor activator, then record the press for the
              // click-vs-drag guard in `handleBarClick`.
              listeners?.onPointerDown?.(e);
              pressPos.current = { x: e.clientX, y: e.clientY };
            }}
            onClick={handleBarClick}
            title={`Drag to reorder ${mesh.name} · click to change mesh colour`}
            aria-label={`Change mesh colour for ${mesh.name} — drag to reorder`}
            className="group/bar flex w-[24px] shrink-0 cursor-grab active:cursor-grabbing items-center justify-center rounded-md hover:bg-bg-card-hover self-stretch"
          >
            <span
              aria-hidden="true"
              style={{ backgroundColor: dimmed ? 'transparent' : meshColor.hex }}
              className={`w-[3px] self-stretch rounded-sm border border-border-strong transition-[width] group-hover/bar:w-[6px] ${dimmed ? 'opacity-30' : ''}`}
            />
          </button>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span className={`font-sans font-semibold text-sm truncate flex-1 ${dimmed ? 'text-text-muted' : 'text-text-primary'}`}>
                {mesh.name}
              </span>
              {drifted && (
                <button
                  type="button"
                  onClick={(e) => { e.stopPropagation(); onOpenWorktreesProbe(mesh.id); }}
                  title={buildDriftTooltip(health!)}
                  aria-label={`Mesh health issue for ${mesh.name}`}
                  className="text-xs font-bold text-status-warning bg-status-warning/15 hover:bg-status-warning/30 rounded-md px-1.5 leading-[18px] transition-colors shrink-0"
                >
                  !
                </button>
              )}
              {behind > 0 && (
                <span
                  title={`${behind} commit${behind === 1 ? '' : 's'} behind upstream`}
                  className="text-xs font-semibold text-status-warning leading-none tabular-nums shrink-0"
                >
                  ↓{behind}
                </span>
              )}
              {syncFailed && (
                <button
                  type="button"
                  onClick={handleSyncRetry}
                  disabled={syncing}
                  title="Last sync from upstream failed — click to retry."
                  aria-label={`Sync failed for ${mesh.name} — click to retry`}
                  className="text-status-error hover:text-text-primary transition-colors shrink-0 disabled:opacity-50"
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className={syncing ? 'animate-spin' : ''}>
                    <polyline points="23 4 23 10 17 10" />
                    <polyline points="1 20 1 14 7 14" />
                    <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10" />
                    <path d="M20.49 15a9 9 0 0 1-14.85 3.36L1 14" />
                  </svg>
                </button>
              )}
            </div>
            {/* Status subtitle: smaller dots + count, and the whole line
                toggles expansion. */}
            <button
              type="button"
              onClick={(e) => { e.stopPropagation(); setExpanded((v) => !v); }}
              aria-expanded={expanded}
              aria-label={expanded ? `Hide agents for ${mesh.name}` : `Show agents for ${mesh.name}`}
              title={expanded ? `Hide agents for ${mesh.name}` : `Show agents for ${mesh.name}`}
              className="mt-px flex w-full items-center gap-1 rounded-md pr-1 py-px text-left hover:bg-bg-card-hover/60 transition-colors"
            >
              {members.length > 0 ? (
                <span role="img" aria-label={dotSummary} className="inline-flex items-center gap-[3px]">
                  {members.map((node) => {
                    const config = getNodeStatusConfig(node);
                    return (
                      <span
                        key={node.id}
                        aria-hidden="true"
                        title={`${node.name} — ${config.title}`}
                        className={`h-1.5 w-1.5 rounded-full ${config.bgColor}`}
                      />
                    );
                  })}
                </span>
              ) : (
                <span className="text-2xs text-text-muted">No agents yet</span>
              )}
              {members.length > 0 && (
                <span className="text-2xs text-text-muted tabular-nums">{members.length}</span>
              )}
              <span aria-hidden="true" className={`inline-block text-2xs text-text-muted transition-transform ${expanded ? 'rotate-90' : ''}`}>▸</span>
            </button>
          </div>
          {/* Spawn sits outside the text column so it centres on the full
              two-line header height. */}
          <div className="flex shrink-0 items-center">
            <NodeCreationForm
              mesh={mesh}
              isDropdownOpen={isDropdownOpen}
              isSpawning={isSpawning}
              providers={providerList}
              onToggleDropdown={onNewNode}
              onSelectProvider={onSelectProvider}
              getDefaultProvider={getDefaultProvider}
            />
          </div>
        </div>
      </div>

      {/* Nodes enclosed in the same card, split by a hairline. */}
      {expanded && members.length > 0 && (
        <div className="mx-2 mb-1.5 border-t border-border-subtle pt-1.5">
          {nodeClusters.map((cluster) => (
            <NodeCluster
              key={cluster.root.id}
              cluster={cluster}
              meshColor={meshColor}
              providerList={providerList}
              onSelectNode={handleSelectNode}
              onDeleteNode={onDeleteNode}
            />
          ))}
        </div>
      )}

      {recolorOpen && (
        <MeshRecolorModal
          meshId={mesh.id}
          meshName={mesh.name}
          currentColor={meshColor.hex}
          onClose={() => setRecolorOpen(false)}
        />
      )}
    </div>
  );
}
