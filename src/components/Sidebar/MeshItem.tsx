import { memo, useState, useEffect, useLayoutEffect, useMemo, useRef, useCallback } from 'react';
import { createPortal } from 'react-dom';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { openUrl } from '@tauri-apps/plugin-opener';
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
import { useMeshGitHubUrl } from '../../hooks/useMeshGitHubUrl';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useAriaMenu } from '../../hooks/useAriaMenu';
import { dropdownId } from '../../lib/dropdownId';
import { getNodeStatusConfig, needsAgentAttention } from '../../lib/status';
import { NodeCluster } from './NodeCluster';
import type { NodeActivityCluster } from '../../lib/nodeActivities';
import { NodeCreationForm } from './NodeCreationForm';
import { MeshRecolorModal } from '../Mesh/MeshRecolorModal';
import type { SpawnOption } from '../../lib/groups';

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
  /** A spawn for this mesh is in flight — the `+ ▾` cluster shows
   *  "Spawning…" and disables to prevent duplicate nodes. */
  isSpawning: boolean;
  /** Issue #1939 — the mesh sits in the sidebar's inactive band (no open
   *  nodes). Presentational only: dimmed text with the colour bar
   *  suppressed. Structure, height, and every affordance (spawn, reorder,
   *  context menu) are unchanged. Optional so existing call sites are
   *  unaffected; absent means active. */
  dimmed?: boolean;
  providerList: SpawnOption[];
  onSelectMesh: (id: number) => void;
  onNewNode: (mesh: Mesh) => void;
  onSelectProvider: (mesh: Mesh, providerId: string, useWorktree?: boolean, configurationId?: string) => void;
  // Issue #376: opens the unified Probe Panel on the 📁 (Project Files) tab
  // for this mesh. Replaces the legacy `onToggleFileExplorer` prop, which
  // toggled the deleted SessionView left-pane `FileExplorerPanel`.
  onOpenFilesProbe: () => void;
  /** Paired agents share one Node Activity card, and the sidebar clusters them
   *  under a connector rail so the pairing is visible here too. Derived in
   *  `Sidebar` from the same `activityRootId` resolver the grid uses (via
   *  `clusterActivityNodes`), so the two surfaces cannot disagree about
   *  membership. Replaces the flat `meshNodes` list; each cluster carries its
   *  own ordered members. */
  nodeClusters: NodeActivityCluster[];
  onActivateNode: (id: number) => void;
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
   * Issue #375 — the right-click "Properties" item jumps to the Probe
   * Panel on the ⚙️ Mesh Properties tab. The handler is responsible for
   * selecting the mesh (so `useProbeContext` resolves to the right row)
   * before flipping the probe open.
   */
  onOpenPropertiesProbe: (meshId: number) => void;
  /**
   * Issue #767 — the drift `!` badge routes to the 🌳 Worktree Manager
   * tab (where the HealthBlock + Restore/Free actions live), not to
   * the ⚙️ Properties tab. The Properties tab is purely configuration
   * and has no recovery controls.
   */
  onOpenWorktreesProbe: (meshId: number) => void;
}

/// Issue #1748 — the sidebar re-rendered every row on every node update:
/// `Sidebar` maps all meshes on each store change, so without memo every
/// `MeshItem` (and through it every `NodeItem`) re-rendered on any node
/// patch. The comparator below bails out unless this mesh's own data
/// changed. `nodeClusters` is compared element-wise (not by array identity):
/// `Sidebar`'s grouped map rebuilds the per-mesh arrays on each store
/// update while the store's shallow reconciliation (issue #1384) preserves
/// per-node references, so identical member references mean "nothing in
/// this mesh changed". All callbacks come from `Sidebar`'s `useCallback`
/// set, so reference equality on them holds in the steady state.
function sameClusterLists(left: NodeActivityCluster[], right: NodeActivityCluster[]): boolean {
  return left.length === right.length && left.every((cluster, index) => {
    const other = right[index];
    return cluster.root === other.root
      && cluster.paired === other.paired
      && cluster.handGrouped === other.handGrouped
      && cluster.members.length === other.members.length
      && cluster.members.every((node, memberIndex) => node === other.members[memberIndex]);
  });
}

function areMeshItemPropsEqual(previous: MeshItemProps, next: MeshItemProps): boolean {
  return (
    previous.mesh === next.mesh
    && previous.isSelected === next.isSelected
    && previous.isDropdownOpen === next.isDropdownOpen
    && previous.isSpawning === next.isSpawning
    && previous.dimmed === next.dimmed
    && previous.providerList === next.providerList
    && sameClusterLists(previous.nodeClusters, next.nodeClusters)
    && previous.onSelectMesh === next.onSelectMesh
    && previous.onNewNode === next.onNewNode
    && previous.onSelectProvider === next.onSelectProvider
    && previous.onOpenFilesProbe === next.onOpenFilesProbe
    && previous.onOpenIssuesProbe === next.onOpenIssuesProbe
    && previous.onOpenSessionHistoryProbe === next.onOpenSessionHistoryProbe
    && previous.onOpenPropertiesProbe === next.onOpenPropertiesProbe
    && previous.onOpenWorktreesProbe === next.onOpenWorktreesProbe
    && previous.onActivateNode === next.onActivateNode
    && previous.selectMesh === next.selectMesh
    && previous.onDeleteNode === next.onDeleteNode
    && previous.getDefaultProvider === next.getDefaultProvider
  );
}

export const MeshItem = memo(MeshItemView, areMeshItemPropsEqual);

/// A mesh counts as hot when any member needs the user or errored.
function isHotMesh(nodes: readonly AgentNode[]): boolean {
  return nodes.some((node) => needsAgentAttention(node.status) || node.status === 'error');
}

function MeshItemView({
  mesh,
  isSelected,
  isDropdownOpen,
  isSpawning,
  dimmed = false,
  providerList,
  onSelectMesh,
  onNewNode,
  onSelectProvider,
  onOpenFilesProbe,
  nodeClusters,
  onActivateNode,
  selectMesh,
  onDeleteNode,
  onOpenIssuesProbe,
  onOpenSessionHistoryProbe,
  getDefaultProvider,
  onOpenPropertiesProbe,
  onOpenWorktreesProbe,
}: MeshItemProps) {
  // The colour bar IS the reorder handle (click = picker, drag = reorder)
  // through the same dnd-kit context the sidebar provides. The 5px
  // pointer-sensor grace in `Sidebar` lets clicks pass through; the
  // press-position guard in `handleBarClick` swallows the stray click a
  // real drag leaves behind.
  const {
    setNodeRef,
    transform,
    transition,
    isDragging,
    attributes,
    listeners,
  } = useSortable({ id: mesh.id });
  // Click-vs-drag on the bar: AT-style activation (`detail === 0`, no pointer
  // sequence) always opens the picker, so a stale press record can never
  // affect it; a mouse release within 5px of the recorded press is a picker
  // click; anything further travelled means a drag just ran and the trailing
  // click is swallowed. The record needs no clearing: the next pointerdown
  // always overwrites it, and a bar click with no recorded press cannot be a
  // drag remnant (a real press always precedes it). Every branch stops
  // propagation — the swallowed branch must not bubble up and select the mesh.
  // (jsdom + userEvent cannot drive clicks through attached dnd-kit
  // activators, so the click path is covered by fireEvent-sequence tests here
  // and by a real CDP click in the dev-view steps.)
  const pressPos = useRef<{ x: number; y: number } | null>(null);

  const handleBarClick = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (e.detail === 0) {
      setRecolorOpen(true);
      return;
    }
    const start = pressPos.current;
    pressPos.current = null;
    if (!start) {
      setRecolorOpen(true);
      return;
    }
    if (Math.hypot(e.clientX - start.x, e.clientY - start.y) <= 5) {
      setRecolorOpen(true);
    }
  };
  // Keyboard parity for the merged bar. dnd-kit's KeyboardSensor claims
  // Enter and Space as its pickup keys, so Enter is intercepted here to
  // open the picker (what the pre-merge round swatch button did) and every
  // other key is chained straight to the sensor activator — Space still
  // picks the row up for keyboard reordering.
  const handleBarKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      e.stopPropagation();
      setRecolorOpen(true);
      return;
    }
    listeners?.onKeyDown?.(e);
  };
  const meshColor = useMemo(() => getMeshColor(mesh.id, mesh.color), [mesh.id, mesh.color]);
  const [recolorOpen, setRecolorOpen] = useState(false);
  const { branchStatus, refresh: refreshBranchStatus } = useGitBranchStatus(mesh.path);
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
  // line. Nodes often arrive after first mount (store hydration), so heat is
  // tracked continuously until the user touches the toggle — afterwards the
  // toggle owns the state and heat changes never fight the user.
  const [expanded, setExpanded] = useState(() => isHotMesh(members));
  const userToggled = useRef(false);
  useEffect(() => {
    if (!userToggled.current && isHotMesh(members)) setExpanded(true);
  }, [members]);
  const toggleExpanded = useCallback(() => {
    userToggled.current = true;
    setExpanded((v) => !v);
  }, []);

  // Failure-only sync indicator (no always-on button): syncs are automatic
  // (background sync per ADR 0020 + spawn-time auto-sync), so a permanent
  // header button duplicated the Regenerate icon for an action the user
  // rarely needs. The badge appears ONLY when the backend reported a failed
  // sync for this mesh — i.e. it may be going stale — and the card has no
  // textual status line, so the badge is itself the retry affordance.
  const [syncFailed, setSyncFailed] = useState(false);
  const [syncing, setSyncing] = useState(false);

  // Light the failure icon only for THIS mesh: the event is app-global and
  // carries the mesh path that failed, so match through the same
  // path-normalisation helper every other mesh-scoped subscriber uses
  // (slash/case/worktree-aware). A successful sync below clears it.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    listen<MeshSyncWarningPayload>('mesh-sync-warning', (event) => {
      if (pathMatchesGitEvent({ path: event.payload.mesh_path }, mesh.path)) {
        setSyncFailed(true);
      }
    }).then((fn) => {
      // The mesh (or the whole sidebar) can unmount before the IPC-side
      // subscription resolves — unsubscribe immediately in that case.
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [mesh.path]);

  // One sync path behind both entry points: the failure badge in the header
  // and "Force sync from upstream" in the context menu. A success clears the
  // badge and recomputes the behind count (the pull may have advanced HEAD);
  // a failure leaves the badge lit, which is the only feedback the card
  // gives by design.
  const handleSync = async () => {
    if (syncing) return;
    setSyncing(true);
    try {
      await gitSync(mesh.path);
      setSyncFailed(false);
      refreshBranchStatus();
    } catch (err) {
      console.error('Sync failed:', err);
    } finally {
      setSyncing(false);
    }
  };

  // Issue #1748 — one id-keyed select handler for every row in this mesh,
  // stable across renders (both captured actions are stable), so the
  // memoized `NodeItem` rows can cover it by reference instead of
  // receiving a fresh closure per row per render.
  // Single mode stays single (wayfinder #982 / #983): it renders
  // the active node, so the click retargets the solo view
  // automatically — this replaces the old setMaximizedNode
  // retarget. In any grid mode we also select the node's mesh,
  // which flips the canvas to Mesh Grid via the uiStore sync
  // (calling selectMesh unconditionally would break out of
  // Single). Ctrl+Arrow from Single still exits in App.tsx —
  // keyboard parity follow-up is #987.
  const handleSelectNode = useCallback((nodeId: number, meshId: number) => {
    onActivateNode(nodeId);
    if (useUIStore.getState().viewMode !== 'single') {
      selectMesh(meshId);
    }
  }, [onActivateNode, selectMesh]);

  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  // Issue #735 — the menu container ref lets us measure its rendered size
  // for clamping; the trigger ref remembers the row that opened the menu so
  // Escape can return focus there.
  //
  // Issue #837 — keyboard nav (Escape/Tab/Arrow/Home/End + auto-focus on
  // open) is the shared `useAriaMenu` hook below.
  const menuRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLDivElement>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  // View on GitHub — only shown when the mesh's `origin` resolves to a
  // github.com URL. The hook fires the IPC on mount so by the time the
  // user right-clicks the value is in the cache; non-GitHub meshes get
  // `url === null` and the menu item is simply not rendered.
  const { url: githubUrl } = useMeshGitHubUrl(mesh.id, mesh.path);
  // Render-time item count: 5 always-present items + the conditional
  // 6th when the mesh has a GitHub origin. The hook uses this as its
  // `itemCount` so a non-GitHub mesh's menu correctly wraps at 5 and a
  // GitHub mesh's wraps at 6.
  const itemCount = 5 + (githubUrl ? 1 : 0);

  // Issue #735 — close the menu and return focus to the trigger. Used by
  // Escape and any menuitem click so the user's focus stays predictable
  // across menu interactions. The `requestAnimationFrame` runs after the
  // unmount so the trigger ref is still attached when the focus() lands.
  const closeContextMenu = () => {
    const trigger = triggerRef.current;
    setContextMenu(null);
    requestAnimationFrame(() => trigger?.focus({ preventScroll: true }));
  };

  // Issue #837 — the WAI-ARIA keyboard handler + auto-focus on open
  // are the shared `useAriaMenu` hook. The hook attaches the
  // document-level keydown listener only while `enabled` is true
  // (gated on `contextMenu` being open) and re-runs the auto-focus
  // layout effect on every open flip — so `closeContextMenu()`'s
  // trigger-focus return is the only thing left here.
  useAriaMenu({
    rootRef: menuRef,
    itemCount,
    activeIndex,
    setActiveIndex,
    onClose: closeContextMenu,
    enabled: !!contextMenu,
  });

  // Issue #814 — outside-mousedown close goes through the shared
  // `useClickOutside` hook. `mesh.id` scopes the selector so two
  // sidebar meshes with open context menus don't interfere.
  //
  // Issue #1264 — prefix the selector with the surface tag so a
  // mesh-keyed context menu can't collide with a node-keyed context
  // menu on the same numeric id (both autoincrement from the same
  // SQLite sequence, so collisions are routine). Mirrors the prefix
  // applied in `NodeItem` and the Terminal context menu.
  useClickOutside<string>(contextMenu ? dropdownId('mesh', mesh.id) : null, () => closeContextMenu());

  // Issue #735 — viewport clamping. Runs after the menu mounts so we can
  // read its rendered size; pushes the position back into state if it
  // would overflow the right or bottom edge. `useLayoutEffect` keeps the
  // adjustment off-screen so the user never sees the over-large position.
  //
  // Issue #837 — this `setState` repositioning shape is OUT OF SCOPE for
  // the shared `useViewportClamp` hook (which only handles `translateY`).
  // The mesh context menu is anchored at the right-click point (not at a
  // trigger), so a `transform` doesn't help — we need to rewrite the
  // `{x, y}` state object. Leaving it alone keeps the behaviour
  // identical to pre-#837.
  useLayoutEffect(() => {
    if (!contextMenu) return;
    const el = menuRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const MARGIN = 4;
    // Compute the deltas needed to bring the rect inside the viewport;
    // only apply when an actual overflow exists.
    const overX = rect.right - (vw - MARGIN);
    const overY = rect.bottom - (vh - MARGIN);
    if (overX <= 0 && overY <= 0) return;
    const nextX = Math.max(MARGIN, contextMenu.x - (overX > 0 ? overX : 0));
    const nextY = Math.max(MARGIN, contextMenu.y - (overY > 0 ? overY : 0));
    // No-op guard — without this, a stubbed `getBoundingClientRect` that
    // doesn't track the rendered position can put us in an infinite setState
    // loop (effect re-fires because the state object identity changes).
    if (nextX === contextMenu.x && nextY === contextMenu.y) return;
    setContextMenu({ x: nextX, y: nextY });
  }, [contextMenu]);

  const dotSummary = useMemo(() => {
    if (members.length === 0) return 'No agents yet';
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
      // Stable seam for the sidebar's row-level tests and e2e steps.
      data-mesh-card={mesh.id}
      className={`mb-1.5 rounded-lg border bg-bg-card/60 ${drifted ? 'border-status-warning/40' : 'border-border-subtle'}`}
    >
      {/* Two-line header. Line 1 is identity + actions; line 2 is status
          (dots + count, no textual labels) and doubles as the expand toggle,
          so no chevron takes up horizontal space. */}
      {/* Header is a plain clickable container like the pre-merge row
          (issue #735): tabIndex -1 keeps it out of the natural Tab order
          (and gives the context menu a focus target to return to), and it
          carries no role so the bar/dots/drift/sync/spawn buttons nested
          inside are valid interactive descendants. Keyboard users drive
          those buttons directly. */}
      <div
        ref={triggerRef}
        tabIndex={-1}
        onClick={() => onSelectMesh(mesh.id)}
        onContextMenu={(e) => {
          e.preventDefault();
          setContextMenu({ x: e.clientX, y: e.clientY });
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
            onKeyDown={handleBarKeyDown}
            onClick={handleBarClick}
            title={`Drag to reorder ${mesh.name} · click or Enter to change mesh colour`}
            aria-label={`Change mesh colour for ${mesh.name} — Enter to open, Space to pick up for reordering`}
            // Issue #727, same documented trade as the pre-merge drag
            // handle: announce the bar as "sortable" (a positional item that
            // can be reordered) rather than dnd-kit's default "draggable".
            aria-roledescription="sortable"
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
              <span
                id={`mesh-item-name-${mesh.id}`}
                className={`font-sans font-semibold text-sm truncate flex-1 ${dimmed ? 'text-text-muted' : 'text-text-primary'}`}
              >
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
                  onClick={(e) => { e.stopPropagation(); void handleSync(); }}
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
              onClick={(e) => { e.stopPropagation(); toggleExpanded(); }}
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

      {/* Agent nodes within this mesh, clustered by Node Activity so paired
          agents read as one card with sub-agents. A lone node renders as a
          bare row with no rail. Enclosed in this mesh's own card and shown
          only while expanded (the dots line above toggles it). */}
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

      {/* Context menu — periphery actions.
          Portaled to `document.body` so `position:fixed` is not
          retargeted by this row's dnd-kit `transform` (sortable
          containing block). */}
      {contextMenu && createPortal(
        <div
          ref={menuRef}
          // Issue #814 — scoped attribute for `useClickOutside`. `mesh.id`
          // ensures sibling meshes' menus don't satisfy this menu's
          // "inside" check.
          // Issue #1264 — prefix with the surface tag so a mesh-keyed
          // menu can't collide with a node- or terminal-keyed menu that
          // shares the same numeric id.
          data-dropdown-for={dropdownId('mesh', mesh.id)}
          // Issue #735 — WAI-ARIA `menu` role; `aria-labelledby` points at
          // the mesh-name span added above so screen readers can announce
          // the menu's accessible name. Viewport clamping happens in the
          // `useLayoutEffect` above; `style={{ top, left }}` reflects the
          // potentially-repositioned coordinates.
          role="menu"
          aria-labelledby={`mesh-item-name-${mesh.id}`}
          className="fixed bg-bg-overlay border border-border-default rounded-md shadow-md animate-scale-in origin-top-left z-[100] py-1 min-w-[180px]"
          style={{ top: contextMenu.y, left: contextMenu.x }}
          onMouseDown={(e) => e.stopPropagation()}
          onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); }}
        >
          <button
            // Roving tabindex — only the active item is in the Tab order.
            role="menuitem"
            tabIndex={activeIndex === 0 ? 0 : -1}
            onClick={() => { closeContextMenu(); onOpenPropertiesProbe(mesh.id); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card-hover flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3"/>
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>
            </svg>
            Properties
          </button>
          <button
            role="menuitem"
            tabIndex={activeIndex === 1 ? 0 : -1}
            onClick={() => { closeContextMenu(); onOpenFilesProbe(); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card-hover flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>
            </svg>
            File Explorer
          </button>
          <button
            role="menuitem"
            tabIndex={activeIndex === 2 ? 0 : -1}
            onClick={() => { closeContextMenu(); void handleSync(); }}
            disabled={syncing}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card-hover flex items-center gap-2 disabled:opacity-50"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className={syncing ? 'animate-spin' : ''}>
              <polyline points="23 4 23 10 17 10"/>
              <polyline points="1 20 1 14 7 14"/>
              <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10"/>
              <path d="M20.49 15a9 9 0 0 1-14.85 3.36L1 14"/>
            </svg>
            {syncing ? 'Syncing…' : 'Force sync from upstream'}
          </button>
          <button
            role="menuitem"
            tabIndex={activeIndex === 3 ? 0 : -1}
            onClick={() => { closeContextMenu(); onOpenSessionHistoryProbe(mesh.id); }}
            title="Archived Nodes"
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card-hover flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="10"/>
              <polyline points="12 6 12 12 16 14"/>
            </svg>
            Archive
          </button>
          <button
            role="menuitem"
            tabIndex={activeIndex === 4 ? 0 : -1}
            onClick={() => { closeContextMenu(); onOpenIssuesProbe(mesh.id); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card-hover flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="10"/>
              <line x1="12" y1="8" x2="12" y2="16"/>
              <line x1="8" y1="12" x2="16" y2="12"/>
            </svg>
            GitHub Issues
          </button>
          {/* "View on GitHub" — only rendered when the mesh has a
              github.com origin (conditional render so non-GitHub meshes
              keep their 5-item menu and the keyboard-nav count in
              `itemCount` stays accurate). The arrow-out-of-a-box icon
              matches the `↗` glyph used in the rest of the codebase
              (SafeLink, GridNodeHeader's open-PR chip). The click goes
              through `openUrl()` per the knowledge primer's anti-pattern
              note (Tauri 2 silently drops `target="_blank"` without an
              explicit capability we don't grant). */}
          {githubUrl && (
            <button
              role="menuitem"
              tabIndex={activeIndex === 5 ? 0 : -1}
              onClick={() => {
                closeContextMenu();
                openUrl(githubUrl).catch(console.error);
              }}
              className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card-hover flex items-center gap-2"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>
                <polyline points="15 3 21 3 21 9"/>
                <line x1="10" y1="14" x2="21" y2="3"/>
              </svg>
              View on GitHub
            </button>
          )}
        </div>,
        document.body,
      )}
    </div>
  );
}
