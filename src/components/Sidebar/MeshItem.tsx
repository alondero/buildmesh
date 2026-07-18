import { useState, useLayoutEffect, useRef } from 'react';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { Mesh } from '../../stores/meshStore';
import type { AgentNode } from '../../stores/agentNodeStore';
import { useUIStore } from '../../stores/uiStore';
import { getMeshColor } from '../../lib/meshColors';
import { gitSync } from '../../lib/tauri';
import type { MeshHealth } from '../../lib/tauri';
import { useGitBranchStatus } from '../../hooks/useGitBranchStatus';
import { useMeshHealth } from '../../hooks/useMeshHealth';
import { useMeshGitHubUrl } from '../../hooks/useMeshGitHubUrl';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useAriaMenu } from '../../hooks/useAriaMenu';
import { NodeItem } from './NodeItem';
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
  providerList: SpawnOption[];
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

export function MeshItem({
  mesh,
  isSelected,
  isDropdownOpen,
  isSpawning,
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
  onOpenWorktreesProbe,
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
  const [recolorOpen, setRecolorOpen] = useState(false);
  const meshColor = getMeshColor(mesh.id, mesh.color);
  const [syncing, setSyncing] = useState(false);
  const [syncMessage, setSyncMessage] = useState<string | null>(null);
  const syncTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Issue #735 — viewport clamping + ARIA menu keyboard navigation.
  // The menu container ref lets us measure its rendered size for clamping;
  // the trigger ref remembers the row that opened the menu so Escape can
  // return focus there.
  //
  // Issue #837 — keyboard nav (Escape/Tab/Arrow/Home/End + auto-focus on
  // open) is now the shared `useAriaMenu` hook below. The hook reads
  // `itemCount` via a ref it owns and finds menuitems via
  // `querySelectorAll('[role="menuitem"]')`, so the previous
  // `menuItemRefs` + `itemCountRef` mirrors here are no longer needed
  // (the hook handles the live-value closure and the per-item focus
  // walk).
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

  // Issue #735 — close the menu and return focus to the trigger. Used by
  // Escape and any menuitem click so the user's focus stays predictable
  // across menu interactions. The `requestAnimationFrame` runs after the
  // unmount so the trigger ref is still attached when the focus() lands.
  const closeContextMenu = () => {
    const trigger = triggerRef.current;
    setContextMenu(null);
    requestAnimationFrame(() => trigger?.focus());
  };

  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

  // Issue #837 — the WAI-ARIA keyboard handler + auto-focus on open
  // are now the shared `useAriaMenu` hook (#837). The hook attaches
  // the document-level keydown listener only while `enabled` is true
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
  useClickOutside<number>(contextMenu ? mesh.id : null, () => closeContextMenu());

  // Issue #735 — viewport clamping. Runs after the menu mounts so we can
  // read its rendered size; pushes the position back into state if it
  // would overflow the right or bottom edge. `useLayoutEffect` keeps the
  // adjustment off-screen so the user never sees the over-large position.
  //
  // Issue #837 — this `setState` repositioning shape is OUT OF SCOPE for
  // the shared `useViewportClamp` hook (which only handles `translateY`).
  // The MeshItem context menu is anchored at the right-click point
  // (not at a trigger), so a `transform` doesn't help — we need to
  // rewrite the `{x, y}` state object. Leaving it alone keeps the
  // behaviour identical to pre-#837.
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

  return (
    <div ref={setNodeRef} style={style} className="mb-1 group/mesh">
      {/* Mesh header — double height with color accent */}
      <div
        ref={triggerRef}
        // Issue #735 — `tabIndex={-1}` makes the row programmatically
        // focusable so the per-mesh menu can return focus to its trigger
        // on Escape, without putting the row in the natural Tab order.
        tabIndex={-1}
        // The left accent shows the mesh colour. Rendered via inline style
        // (not a Tailwind class) so a user-picked custom hex works, not just
        // the eight palette entries.
        style={{ borderLeftColor: meshColor.hex }}
        className={`border-l-3 rounded-r-md px-2 py-2.5 cursor-pointer transition-colors ${
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
            className="text-text-muted hover:text-text-secondary cursor-grab active:cursor-grabbing text-2xs select-none"
            title="Drag to reorder"
          >
            ⋮⋮
          </span>
          {/* Colour swatch — clicking it opens the colour picker (issue:
              mesh colour picker). stopPropagation so it doesn't also
              select/deselect the mesh row. */}
          <button
            type="button"
            onClick={(e) => { e.stopPropagation(); setRecolorOpen(true); }}
            title="Change mesh colour"
            aria-label="Change mesh colour"
            className="h-3 w-3 shrink-0 rounded-full border border-black/20 hover:scale-125 transition-transform"
            style={{ backgroundColor: meshColor.hex }}
          />
          <span
            id={`mesh-item-name-${mesh.id}`}
            className="font-sans font-semibold text-sm text-text-primary truncate flex-1"
          >
            {mesh.name}
          </span>
          {health && (health.is_drifted || health.base_branch_holder !== null) && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                onOpenWorktreesProbe(mesh.id);
              }}
              title={buildDriftTooltip(health)}
              className="text-xs font-bold text-status-warning bg-status-warning/15 hover:bg-status-warning/30 rounded-md px-1.5 leading-[18px] transition-colors"
              aria-label="Mesh health issue"
            >
              !
            </button>
          )}
          {behind > 0 && (
            <span
              className="text-xs font-semibold text-status-warning leading-none tabular-nums"
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
            isSpawning={isSpawning}
            providers={providerList}
            onToggleDropdown={onNewNode}
            onSelectProvider={onSelectProvider}
            getDefaultProvider={getDefaultProvider}
          />
        </div>
      </div>
      {syncMessage && (
        <div className="ml-2 mr-2 mb-1 px-2 py-1 rounded-md text-xs bg-bg-overlay border border-border-subtle text-text-secondary">
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
          // Issue #774 — the Regenerate submenu shows every available
          // Spawn Option as a picker; threading `providerList` keeps the
          // submenu visually consistent with `ProviderDropdown` (same
          // harness-grouped render, same icons).
          providerList={providerList}
          onSelect={() => {
            setActiveNode(node.id);
            selectMesh(node.mesh_id);
            // Retarget the solo view if one is open. The `null` check
            // preserves "click-while-nothing-maximised leaves maximise
            // null"; the store's idempotency guard handles self-clicks.
            // Deferred: Ctrl+Arrow from maximised still exits in
            // App.tsx:251-256 — revisit keyboard parity as a follow-up
            // so mouse and keyboard agree.
            const currentMaximized = useUIStore.getState().maximizedNodeId;
            if (currentMaximized !== null) {
              useUIStore.getState().setMaximizedNode(node.id);
            }
          }}
          onDelete={(e) => onDeleteNode(e, node.id)}
        />
      ))}

      {recolorOpen && (
        <MeshRecolorModal
          meshId={mesh.id}
          meshName={mesh.name}
          currentColor={meshColor.hex}
          onClose={() => setRecolorOpen(false)}
        />
      )}

      {/* Context menu — periphery actions */}
      {contextMenu && (
        <div
          ref={menuRef}
          // Issue #814 — scoped attribute for `useClickOutside`. `mesh.id`
          // ensures sibling meshes' menus don't satisfy this menu's
          // "inside" check (the previous hand-rolled ref.contains shape
          // was scoped per-instance via the same `mesh.id`-keyed
          // selector inside the closure).
          data-dropdown-for={mesh.id}
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
        >
          <button
            // Roving tabindex — only the active item is in the Tab order.
            role="menuitem"
            tabIndex={activeIndex === 0 ? 0 : -1}
            onClick={() => { closeContextMenu(); onOpenPropertiesProbe(mesh.id); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3"/>
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>
            </svg>
            Properties
          </button>
          <button
            role="menuitem"
            tabIndex={activeIndex === 1 ? 0 : -1}
            onClick={() => { closeContextMenu(); onOpenFilesProbe(); }}
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/>
            </svg>
            File Explorer
          </button>
          <button
            role="menuitem"
            tabIndex={activeIndex === 2 ? 0 : -1}
            onClick={() => { closeContextMenu(); handleSync(); }}
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
            role="menuitem"
            tabIndex={activeIndex === 3 ? 0 : -1}
            onClick={() => { closeContextMenu(); onOpenSessionHistoryProbe(mesh.id); }}
            title="Archived Nodes"
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
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
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
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
              className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>
                <polyline points="15 3 21 3 21 9"/>
                <line x1="10" y1="14" x2="21" y2="3"/>
              </svg>
              View on GitHub
            </button>
          )}
        </div>
      )}
    </div>
  );
}
