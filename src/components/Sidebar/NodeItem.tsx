import { useState, useEffect, useLayoutEffect, useMemo, useRef, useCallback } from 'react';
import { createPortal } from 'react-dom';
import type { AgentNode } from '../../stores/agentNodeStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { getStatusConfig } from '../../lib/status';
import { canResumeSuspendedNode } from '../../lib/suspended';
import { getMeshColor } from '../../lib/meshColors';
import type { SpawnOption } from '../../lib/groups';
import {
  REGENERATE_DISABLED_STATUSES,
  splitRegenerateTargets,
  hasRegenerateTargets as hasRegenerateTargetsHelper,
} from '../../lib/regenerate';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { RegenerateProviderMenu } from '../Providers/RegenerateProviderMenu';
import { InlineEditableText } from '../shared/InlineEditableText';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { useClickOutside } from '../../hooks/useClickOutside';
import { dropdownId } from '../../lib/dropdownId';
import { addToast } from '../../stores/toastStore';
import { formatError } from '../../lib/errorUtils';

// Issue #776 — Regenerate is the entry point for the new "restart this
// node" flow wired up in ticket 03 of #774. We disable it (rather than
// hide it) for statuses where a fresh `spawn_agent` IPC would race the
// in-flight spawn or the backend would reject it. Greyed-out is more
// discoverable than hidden and lets the tooltip explain *why* the
// action is unavailable. `suspended` was removed from this list when
// user-driven recovery for Suspended nodes shipped — the Regenerate
// picker now reuses the existing `decide_resume` rule (same harness
// + `cli_session_id` → resume; else fresh), and the orchestrator
// branches on `should_skip_kill_for_regenerate` to avoid the
// unconditional `on_idle` tail clobbering Suspended→Idle.
//
// Issue #1502 — the disabled-status list now lives in
// `src/lib/regenerate.ts` (shared with `GridNodeHeader`'s toolbar/kebab
// affordances) so both surfaces stay in lockstep.

// Focus a menuitem without scrolling overflow ancestors. Nested inside
// the sidebar list, `.focus()` scrolled that ancestor to the item's
// layout box and the menu jumped. Belt-and-suspenders now that the
// menu is portaled to `document.body`.
function focusWithoutScroll(el: HTMLElement | null | undefined) {
  el?.focus({ preventScroll: true });
}

interface NodeItemProps {
  node: AgentNode;
  meshColor: ReturnType<typeof getMeshColor>;
  isActive: boolean;
  /**
   * Issue #774 / ticket 03 — the Spawn Options available on this mesh.
   * Issue #1502 — the Regenerate submenu renders this list INCLUDING
   * the node's current provider (pinned on top as `Current (<label>)`
   * for in-place kick-start); absent or empty means the submenu has
   * nothing to offer and the parent Regenerate row stays visible but
   * greyed-out (the user can still see the affordance exists).
   */
  providerList?: SpawnOption[];
  onSelect: () => void;
  onDelete: (e: React.MouseEvent) => void;
}

export function NodeItem({ node, meshColor, isActive, providerList, onSelect, onDelete }: NodeItemProps) {
  const config = getStatusConfig(node.status);
  const renameAgentNode = useAgentNodeStore((s) => s.renameAgentNode);
  const spawnAgent = useAgentNodeStore((s) => s.spawnAgent);
  const regenerateAgentNode = useAgentNodeStore((s) => s.regenerateAgentNode);
  // Issue #1306 — "Start Fresh" escape hatch for error nodes with stale session IDs.
  const restartFreshAgent = useAgentNodeStore((s) => s.restartFreshAgent);
  // Pin/Unpin (wayfinder #982 / #985) — the shared store action is the
  // sole mutation path; `node.is_pinned` drives this item's label/icon.
  const toggleNodePinned = useAgentNodeStore((s) => s.toggleNodePinned);
  // Closing a node first runs a worktree safety check that can take seconds on
  // a large repo; until it resolves the row stays on screen, so show a spinner
  // (and stop reacting to clicks) rather than letting the click look ignored.
  const isClosing = useAgentNodeStore((s) => s.closingNodeIds.has(node.id));
  // 'error' is the false-positive status the app-exit / post-pump race
  // leaves behind (see agent/spawn.rs:419-438 vs lib.rs:247-253) — the
  // user never got a chance to actually use the node, so the
  // meaningful action is "retry the spawn", not "delete". The store's
  // spawnAgent passes `cli_session_id` as the resume argument, so a
  // click re-attempts the same --resume the failed auto-resume tried.
  const showRestart = node.status === 'error';
  // Issue #1306 — "Start Fresh" affordance: shown alongside Restart when
  // the node is in Error AND has a cli_session_id (i.e. the session that
  // caused the failure is actually captured). Without cli_session_id the
  // Restart button already starts fresh; the extra button would be a no-op.
  const showStartFresh = node.status === 'error' && node.cli_session_id != null;

  // Same status (`Suspended`) covers two cases that need different
  // affordances:
  //   1. Crash-recovery resume — the agent DID run, captured a
  //      `cli_session_id`, and was parked by `recover_from_crash`
  //      (`session_lifecycle.rs`). The Resume button re-attempts the
  //      resume via `spawn_agent`.
  //   2. Autopilot gate (`autopilot::GateDecision::RequireApproval`) —
  //      the node was parked at creation, no agent ever ran, so
  //      `cli_session_id` is NULL. The autopilot's own "Approve
  //      Sandbox Run" action is the recovery surface; a generic
  //      Resume click here would surface "no CLI session ID is
  //      stored" as a toast.
  // The data column (`cli_session_id`) is the disambiguator — see the
  // shared `canResumeSuspendedNode` helper for the predicate source of
  // truth (used by both `NodeItem` and `GridNodeHeader`).
  const showResume = canResumeSuspendedNode(node);

  // Issue #776 — right-click context menu (Regenerate entry point).
  // Mirrors the MeshItem menu infrastructure (issue #735): the menu
  // container ref drives viewport clamping + ARIA keyboard-nav scoping,
  // the trigger ref lets Escape / outside-click restore focus to the
  // row, and roving tabindex allows arrow keys to move focus between items.
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLDivElement>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  // Issue #774 / ticket 03 — Regenerate submenu state. `submenuOpen`
  // is true while the provider picker is rendered. `submenuItemRefs`
  // holds the provider rows so the keyboard handler can move focus
  // into the submenu on ArrowRight and back out on ArrowLeft (WAI-ARIA
  // menu-with-menubutton pattern).
  const [submenuOpen, setSubmenuOpen] = useState(false);
  const submenuRef = useRef<HTMLDivElement>(null);
  const submenuItemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  // Issue #1502 — in-place regeneration (kick-start): the picker now
  // INCLUDES the node's current provider, pinned to the top as
  // `Current (<label>)`, followed by every other provider grouped by
  // `group_key` (consistent with `ProviderDropdown` and
  // `ArchivedNodesTab`, issue #583 centralisation, ADR-0016). The
  // backend's `decide_resume` already handles `same_harness == true`
  // cleanly, so regenerating onto the same setup kills the process
  // and starts a fresh agent session in the same worktree. Partitioning
  // lives in `src/lib/regenerate.ts` (shared with `GridNodeHeader`).
  const { current: currentTarget, others: alternateTargets } = useMemo(
    () => splitRegenerateTargets(providerList, node.provider),
    [providerList, node.provider],
  );
  // Total number of submenu items (current + alternates). Used by the
  // keyboard-nav handler so ArrowDown/Up inside the submenu wraps
  // around the full picker, not the per-group slice.
  const submenuItemCount = (currentTarget ? 1 : 0) + alternateTargets.length;

  type ContextMenuAction = 'regenerate' | 'startFresh' | 'pin';
  const menuActions: ContextMenuAction[] = useMemo(
    () => ['regenerate', ...(showStartFresh ? (['startFresh'] as const) : []), 'pin'],
    [showStartFresh],
  );

  const getParentMenuItems = useCallback((): HTMLButtonElement[] => {
    const menu = menuRef.current;
    if (!menu) return [];
    return Array.from(
      menu.querySelectorAll<HTMLButtonElement>('button[role="menuitem"]'),
    ).filter((el) => el.closest('[role="menu"]') === menu);
  }, []);

  const isRegenerateDisabled = REGENERATE_DISABLED_STATUSES.includes(node.status);
  // Issue #1502 — the picker now includes the current provider
  // (in-place kick-start), so the trigger is enabled whenever the mesh
  // offers ANY provider — even if that is only the current one. Empty
  // (no providers at all) stays greyed-out with a tooltip explaining
  // the lack, not hidden (mirrors the status-gating pattern above).
  const hasRegenerateTargets = hasRegenerateTargetsHelper(providerList);

  // Close and return focus to the row that opened the menu. Used by
  // Escape and any menuitem click so the user's focus stays
  // predictable across menu interactions. The `requestAnimationFrame`
  // runs after the unmount so the trigger ref is still attached when
  // the focus() lands. Also tears down submenu state so reopening
  // starts from a known-closed picker.
  const closeContextMenu = () => {
    const trigger = triggerRef.current;
    setSubmenuOpen(false);
    setContextMenu(null);
    requestAnimationFrame(() => trigger?.focus({ preventScroll: true }));
  };

  // Issue #778 — in-flight confirmation dialog. Carries both the
  // chosen provider id (the IPC argument) and its display label (for
  // the dialog message). `null` means no dialog open; the picker click
  // sets it, both Confirm and Cancel clear it.
  const [pendingRegenerate, setPendingRegenerate] = useState<{
    providerId: string;
    providerLabel: string;
  } | null>(null);
  const cancelRegenerate = () => setPendingRegenerate(null);
  const confirmRegenerate = () => {
    if (!pendingRegenerate) return;
    const { providerId } = pendingRegenerate;
    setPendingRegenerate(null);
    // Issue #1001 (mirrors `agentNodeStore.deleteAgentNode`): surface a
    // failure through the shared toast pipeline. The previous
    // `console.error` here left the user staring at a menu that
    // closed silently on a backend rejection (the classic case was a
    // `Completed` autopilot node, which the validator used to refuse
    // before the Regenerate-on-Completed fix landed).
    regenerateAgentNode(node.id, providerId).catch((err) => {
      addToast('Regenerate failed', formatError(err), 'error');
    });
  };

  // Issue #774 — invoke `regenerate_agent_node` with the chosen
  // provider and close the menu. Centralised so the submenu hover/click
  // paths and any future shortcut key wire to the same error-handling.
  //
  // Issue #778 — when the node is `running`, an interrupting regenerate
  // drops the agent's in-flight PTY output without warning. The picker
  // row click opens a confirmation dialog instead of firing the IPC
  // directly; for `idle` / `awaiting_input` / `error` the dialog is
  // skipped (no live work to lose). `providerLabel` is the human-readable
  // name interpolated into the dialog message so the user sees exactly
  // which Model Provider they're switching to.
  const pickProvider = (providerId: string, providerLabel: string) => {
    if (isRegenerateDisabled || !hasRegenerateTargets) return;
    closeContextMenu();
    if (node.status === 'running') {
      setPendingRegenerate({ providerId, providerLabel });
      return;
    }
    // See `confirmRegenerate` for the why; this is the non-running
    // branch — every status the picker reaches where a fresh
    // `regenerate_agent_node` IPC is fine without a confirm dialog
    // (idle / awaiting_input / error / suspended / completed). Shares
    // the same toast plumbing so the user always sees a backend
    // rejection instead of a silent menu close.
    regenerateAgentNode(node.id, providerId).catch((err) => {
      addToast('Regenerate failed', formatError(err), 'error');
    });
  };

  // Issue #814 — outside-mousedown close goes through the shared
  // `useClickOutside` hook (#492). Both the parent menu AND the
  // submenu carry `data-dropdown-for={node.id}` (see below), so a
  // click inside either subtree satisfies the hook's
  // `[data-dropdown-for="<id>"]` selector — closing on a sub-internal
  // click is now handled by attribute scoping instead of two ref walks.
  //
  // Issue #1264 — prefix with the surface tag so a node-keyed menu
  // can't collide with a mesh-keyed menu that shares the same numeric
  // id (mesh and node ids both autoincrement from the same SQLite
  // sequence, so collisions are routine).
  useClickOutside<string>(contextMenu ? dropdownId('node', node.id) : null, () => closeContextMenu());

  // Mirror dynamic state into refs so the document-level listener does not
  // tear down and re-attach on every keystroke (mirrors `useAriaMenu.ts:38-42`).
  const activeIndexRef = useRef(activeIndex);
  activeIndexRef.current = activeIndex;
  const submenuOpenRef = useRef(submenuOpen);
  submenuOpenRef.current = submenuOpen;
  const submenuItemCountRef = useRef(submenuItemCount);
  submenuItemCountRef.current = submenuItemCount;

  useEffect(() => {
    if (!contextMenu) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      // WAI-ARIA menu contract: keystrokes only apply while focus is
      // inside the menu. The document-level listener would otherwise
      // hijack arrow / Home / End typed elsewhere on the page. We
      // check `document.activeElement` (not `e.target`) because in
      // jsdom tests events are dispatched on `document` while focus is
      // on a menuitem.
      const menu = menuRef.current;
      const submenu = submenuRef.current;
      const active = document.activeElement;
      const inMenu = menu && active instanceof Node && menu.contains(active);
      const inSubmenu = submenu && active instanceof Node && submenu.contains(active);
      if (!inMenu && !inSubmenu) return;

      if (e.key === 'Escape') {
        e.preventDefault();
        closeContextMenu();
        return;
      }
      if (e.key === 'Tab') {
        // WAI-ARIA menu: Tab leaves the menu and closes it (a modal
        // menu would trap focus, but `role="menu"` is a non-modal
        // popover). Don't preventDefault — let the browser move
        // focus normally.
        closeContextMenu();
        return;
      }
      // Issue #774 — submenu navigation. ArrowRight on a submenu trigger
      // opens the submenu and moves focus to the first provider; ArrowLeft
      // inside the submenu (or on the parent menu while open) closes the
      // submenu and returns focus to the submenu trigger.
      if (e.key === 'ArrowRight' && inMenu && !inSubmenu) {
        if (active?.getAttribute('aria-haspopup') !== 'menu') return;
        e.preventDefault();
        if (submenuItemCountRef.current === 0) return;
        setSubmenuOpen(true);
        // Focus the first submenu item after the next render. Using
        // `queueMicrotask` (not `requestAnimationFrame`) keeps the
        // focus call inside the same event loop turn as the React
        // commit, so a tight `await waitFor(...)` in the test can
        // observe the moved focus in the same tick.
        queueMicrotask(() => focusWithoutScroll(submenuItemRefs.current[0]));
        return;
      }
      if (e.key === 'ArrowLeft' && (inSubmenu || (inMenu && submenuOpenRef.current))) {
        e.preventDefault();
        setSubmenuOpen(false);
        const menuEl = menuRef.current;
        const trigger = menuEl?.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]');
        if (trigger) focusWithoutScroll(trigger);
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (inSubmenu) {
          // Inside the submenu: move focus across the flat provider
          // list with wrap-around. The refs array is a flat
          // concatenation of every group's items, in render order.
          const current = submenuItemRefs.current.findIndex((el) => el === active);
          const start = current === -1 ? 0 : current;
          const next = (start + 1) % Math.max(1, submenuItemRefs.current.length);
          focusWithoutScroll(submenuItemRefs.current[next]);
        } else {
          const parentItems = getParentMenuItems();
          if (parentItems.length === 0) return;
          const current = parentItems.findIndex((el) => el === active);
          const start = current === -1 ? activeIndexRef.current : current;
          const next = (start + 1) % parentItems.length;
          setActiveIndex(next);
          focusWithoutScroll(parentItems[next]);
        }
        return;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (inSubmenu) {
          const current = submenuItemRefs.current.findIndex((el) => el === active);
          const start = current === -1 ? submenuItemRefs.current.length - 1 : current;
          const len = Math.max(1, submenuItemRefs.current.length);
          const next = (start - 1 + len) % len;
          focusWithoutScroll(submenuItemRefs.current[next]);
        } else {
          const parentItems = getParentMenuItems();
          if (parentItems.length === 0) return;
          const current = parentItems.findIndex((el) => el === active);
          const start = current === -1 ? activeIndexRef.current : current;
          const next = (start - 1 + parentItems.length) % parentItems.length;
          setActiveIndex(next);
          focusWithoutScroll(parentItems[next]);
        }
        return;
      }
      if (e.key === 'Home' && !inSubmenu) {
        e.preventDefault();
        const parentItems = getParentMenuItems();
        if (parentItems.length === 0) return;
        setActiveIndex(0);
        focusWithoutScroll(parentItems[0]);
        return;
      }
      if (e.key === 'End' && !inSubmenu) {
        e.preventDefault();
        const parentItems = getParentMenuItems();
        if (parentItems.length === 0) return;
        const last = parentItems.length - 1;
        setActiveIndex(last);
        focusWithoutScroll(parentItems[last]);
        return;
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [contextMenu, getParentMenuItems]);

  // Issue #776 — viewport clamping. Runs after the menu mounts so we
  // can read its rendered size; pushes the position back into state if
  // it would overflow the right or bottom edge. `useLayoutEffect` keeps
  // the adjustment off-screen so the user never sees the over-large
  // position. The no-op guard prevents a stubbed
  // `getBoundingClientRect` (which doesn't track the rendered
  // position) from putting us in an infinite setState loop.
  useLayoutEffect(() => {
    if (!contextMenu) return;
    const el = menuRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const MARGIN = 4;
    const overX = rect.right - (vw - MARGIN);
    const overY = rect.bottom - (vh - MARGIN);
    if (overX <= 0 && overY <= 0) return;
    const nextX = Math.max(MARGIN, contextMenu.x - (overX > 0 ? overX : 0));
    const nextY = Math.max(MARGIN, contextMenu.y - (overY > 0 ? overY : 0));
    if (nextX === contextMenu.x && nextY === contextMenu.y) return;
    setContextMenu({ x: nextX, y: nextY });
  }, [contextMenu]);

  // Issue #776 — on open, reset the roving index and move focus to the
  // first menuitem so keyboard nav starts somewhere. `useLayoutEffect`
  // (not `useEffect` + setTimeout) — fires synchronously after commit
  // so subsequent arrow-key presses don't race a deferred focus call.
  useLayoutEffect(() => {
    if (!contextMenu) return;
    setActiveIndex(0);
    const parentItems = getParentMenuItems();
    if (parentItems[0]) focusWithoutScroll(parentItems[0]);
  }, [contextMenu !== null, getParentMenuItems]);

  return (
    <div
      ref={triggerRef}
      data-session-item
      data-session-id={node.id}
      // `tabIndex={-1}` makes the row programmatically focusable so
      // the menu can return focus to its trigger on Escape, without
      // putting the row in the natural Tab order.
      tabIndex={-1}
      onClick={isClosing ? undefined : onSelect}
      onContextMenu={(e) => {
        e.preventDefault();
        setContextMenu({ x: e.clientX, y: e.clientY });
      }}
      aria-busy={isClosing}
      style={{ backgroundColor: isActive ? undefined : `${meshColor.hex}40` }}
      className={`
        pl-3 pr-1 py-1.5 rounded-md text-sm mb-0.5 flex items-center gap-2 group/node
        ${isClosing ? 'opacity-50 pointer-events-none cursor-default' : 'cursor-pointer'}
        ${isActive ? 'border border-accent-cyan/50' : 'hover:brightness-125 border border-transparent'}
      `}
    >
      <span
        className={`${config.color} inline-flex h-3 w-3 shrink-0 items-center justify-center text-xs leading-none`}
        title={config.label}
      >
        {config.dot}
      </span>
      {/* Issue #1364 §3 — node-level hook-health warning (see GridNodeHeader). */}
      {node.signal_health === 'unavailable' && (
        <span
          role="img"
          aria-label="Attention signal unavailable"
          title="Attention signal unavailable — the agent's lifecycle hook could not be installed or reached; watch the terminal directly."
          className="text-status-warning text-xs leading-none shrink-0"
        >
          ⚠
        </span>
      )}
      <ProviderIcon providerId={node.provider} className="h-3 w-3 opacity-90" />
      <InlineEditableText
        // `id` anchors the menu's `aria-labelledby` to a name-only
        // span (not the whole row, whose textContent would include
        // icons + the inline menu). Mirrors MeshItem's
        // `mesh-item-name-${id}` (issue #735).
        id={`node-item-name-${node.id}`}
        value={node.name}
        onCommit={(next) => renameAgentNode(node.id, next)}
        className="flex-1 truncate text-text-primary font-sans text-left text-sm"
      />
      {showRestart && (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            spawnAgent(node.id, node.provider).catch((err) => {
              addToast('Restart failed', formatError(err), 'error');
            });
          }}
          className="text-text-muted hover:text-status-warning text-xs px-1 transition-colors opacity-0 group-hover/node:opacity-100 group-focus-within/node:opacity-100 focus-visible:opacity-100"
          title={
            node.cli_session_id != null
              ? 'Retry resume with existing session'
              : 'Restart agent'
          }
          aria-label={
            node.cli_session_id != null
              ? `Retry resume for ${node.name}`
              : `Restart ${node.name}`
          }
          data-testid="restart-button"
        >
          ↻
        </button>
      )}
      {showResume && (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            // `spawnAgent` reads `cli_session_id` from the store row and
            // passes it as the `resume` arg. For adapters that don't
            // support resume (OpenCode, Terminal) the backend now falls
            // through to Fresh (spawn.rs:supports_resume fall-through)
            // instead of erroring; for adapters that DO, the captured
            // `cli_session_id` is honoured.
            spawnAgent(node.id, node.provider).catch((err) => {
              addToast('Resume failed', formatError(err), 'error');
            });
          }}
          // Mirrors the inline Restart button's hover/focus surface so
          // the two affordances feel like siblings; the violet accent
          // matches the Suspended status dot so a user hovering a
          // Suspended-vs-error row gets a colour cue tied to the
          // status.
          className="text-text-muted hover:text-accent-violet text-xs px-1 transition-colors opacity-0 group-hover/node:opacity-100 group-focus-within/node:opacity-100 focus-visible:opacity-100"
          title="Resume agent"
          aria-label={`Resume ${node.name}`}
          data-testid="resume-button"
        >
          ↻
        </button>
      )}
      {isClosing ? (
        <span
          className="text-text-muted text-xs px-1 flex items-center"
          title="Closing…"
          aria-label="Closing"
        >
          <span className="inline-block h-3 w-3 animate-spin rounded-full border border-current border-t-transparent" />
        </span>
      ) : (
        <button
          type="button"
          onClick={onDelete}
          className="text-text-muted hover:text-status-error text-xs px-1 transition-colors opacity-0 group-hover/node:opacity-100 group-focus-within/node:opacity-100 focus-visible:opacity-100"
          title="Delete node"
          aria-label={`Delete ${node.name}`}
        >
          ×
        </button>
      )}

      {/* Issue #776 — context menu (Regenerate entry point).
          `onMouseDown stopPropagation` keeps row clicks from bubbling
          to the document-level close handler when the user clicks an
          item (otherwise the click would close the menu AND fire the
          row's `onSelect`).
          Portaled to `document.body` so `position:fixed` stays in
          viewport coordinates. Nested inside the row, `hover:brightness-125`
          (`filter`) and the parent MeshItem's dnd-kit `transform`
          retarget `fixed` onto those boxes. */}
      {contextMenu && createPortal(
        <div
          ref={menuRef}
          // Issue #814 — scoped attribute for `useClickOutside`. The
          // submenu carries the same value (below), so a click inside
          // either subtree matches `[data-dropdown-for="<id>"]` and the
          // hook treats both as "inside".
          // Issue #1264 — surface prefix matches the `useClickOutside`
          // call above so the selector resolves to the same DOM.
          data-dropdown-for={dropdownId('node', node.id)}
          role="menu"
          aria-labelledby={`node-item-name-${node.id}`}
          className="fixed bg-bg-overlay border border-border-default rounded-md shadow-md animate-scale-in origin-top-left z-[100] py-1 min-w-[180px]"
          style={{ top: contextMenu.y, left: contextMenu.x }}
          onMouseDown={(e) => e.stopPropagation()}
          onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); }}
        >
          {/* Issue #774 — Regenerate submenu container. The wrapper
              div shares `onMouseEnter` / `onMouseLeave` so the cursor
              can move from the parent button into the submenu
              (positioned to the right) without the gap triggering a
              close. Hovering opens the picker; clicking the parent
              toggles it (tap/touch parity). */}
          <div
            className="relative"
            onMouseEnter={() => {
              if (!isRegenerateDisabled) setSubmenuOpen(true);
            }}
            onMouseLeave={() => setSubmenuOpen(false)}
          >
            <button
              // Roving tabindex — only the active item is in the Tab
              // order. The Regenerate item is disabled for the
              // "race-the-spawn / backend-rejects" statuses (see
              // `REGENERATE_DISABLED_STATUSES`) AND when the picker
              // has no providers at all to offer. The click handler
              // short-circuits as a second guard so a programmatic
              // .click() can't bypass the disabled state.
              role="menuitem"
              aria-haspopup="menu"
              aria-expanded={submenuOpen}
              tabIndex={menuActions[activeIndex] === 'regenerate' ? 0 : -1}
              disabled={isRegenerateDisabled || !hasRegenerateTargets}
              onClick={() => {
                if (isRegenerateDisabled || !hasRegenerateTargets) return;
                // Always open — never toggle. The submenu also opens
                // on hover via the wrapper's `onMouseEnter`; toggling
                // here would close the picker the moment a real user
                // (or `userEvent.click` in tests) clicks the row
                // they just hovered. To close, the user moves the
                // cursor away (`onMouseLeave`) or presses Escape.
                setSubmenuOpen(true);
              }}
              title={
                isRegenerateDisabled
                  // Use the raw status name rather than `config.label`
                  // so the tooltip stays lowercase and consistent with the
                  // other machine status names (e.g. "while suspended").
                  ? `Regenerate unavailable while ${node.status}`
                  : !hasRegenerateTargets
                    ? 'No providers are available on this mesh'
                    : submenuOpen
                      ? 'Hide provider picker'
                      : 'Pick a Model Provider for this node (including current to kick-start)'
              }
              data-testid="regenerate-trigger"
              className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:bg-transparent"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/>
                <path d="M21 3v5h-5"/>
                <path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/>
                <path d="M3 21v-5h5"/>
              </svg>
              Regenerate
              {/* ▸ marks this as a submenu entry point — the picker
                  opens on hover / ArrowRight / click. Spacer pushes
                  the glyph to the right edge to match the WAI-ARIA
                  pattern. */}
              <span aria-hidden="true" className="ml-auto">▸</span>
            </button>
            {/* Issue #774 — provider picker submenu. Positioned
                absolutely to the right of the parent (-ml-1 overlaps
                by 1px so the cursor's path from parent into the
                picker never crosses the wrapper's mouseleave edge).
                `role="menu"` keeps it a peer of the parent menu so
                screen readers announce "menu: <picker label>". Each
                row carries `role="menuitem"` + an `id` matching the
                pattern the existing v1 menu uses (the keyboard-nav
                handler scans `submenuItemRefs.current` in render
                order, so the id pattern is just for tests).
                Issue #1502 — rows render via the shared
                `RegenerateProviderMenu` (current pinned on top for
                in-place kick-start, then alternates grouped by
                harness) so the sidebar and `GridNodeHeader`
                pickers never drift. */}
            {submenuOpen && (
              <div
                // Refresh the submenu item refs every time the
                // submenu mounts (or the inner buttons re-render).
                // Querying the rendered DOM after mount avoids the
                // `ref.current.push(el)` accumulation trap — without
                // a reset, a re-render would leave stale entries
                // pointing at unmounted buttons, and the keyboard
                // handler's `findIndex(active)` would skip them.
                ref={(el) => {
                  submenuRef.current = el;
                  if (el) {
                    submenuItemRefs.current = Array.from(
                      el.querySelectorAll<HTMLButtonElement>('button[role="menuitem"]'),
                    );
                  } else {
                    submenuItemRefs.current = [];
                  }
                }}
                role="menu"
                aria-label="Pick target provider"
                data-testid="regenerate-submenu"
                // Issue #814 — same scoped attribute as the parent menu
                // so `useClickOutside` treats clicks inside the
                // submenu as "inside" (closing on a sub-internal click
                // would make the picker un-clickable).
                // Issue #1264 — same surface prefix as the parent
                // menu so the `useClickOutside` selector resolves
                // across both the parent and submenu.
                data-dropdown-for={dropdownId('node', node.id)}
                className="absolute left-full top-0 -ml-1 min-w-[200px] bg-bg-overlay border border-border-default rounded-md shadow-md py-1 z-[101]"
                // `onMouseDown stopPropagation` mirrors the parent
                // menu — without it, a click inside the submenu
                // bubbles to the document-level mousedown handler
                // and the menu closes BEFORE the button's onClick
                // fires.
                onMouseDown={(e) => e.stopPropagation()}
              >
                <RegenerateProviderMenu
                  providers={providerList ?? []}
                  currentProviderId={node.provider}
                  onPick={pickProvider}
                />
              </div>
            )}
          </div>
          {/* Issue #1306 — "Start Fresh" context menu action: available when
              the node is in Error status and has a captured cli_session_id.
              Allows the user to explicitly discard the dead session ID and boot
              fresh without having to delete the node or switch providers. */}
          {showStartFresh && (
            <button
              type="button"
              role="menuitem"
              tabIndex={menuActions[activeIndex] === 'startFresh' ? 0 : -1}
              onClick={() => {
                closeContextMenu();
                restartFreshAgent(node.id).catch((err) => {
                  addToast('Start Fresh failed', formatError(err), 'error');
                });
              }}
              title="Discard stale session and boot a fresh agent"
              data-testid="context-start-fresh"
              className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" />
                <path d="M3 3v5h5" />
              </svg>
              Start Fresh
            </button>
          )}
          {/* Pin/Unpin (wayfinder #982 / #985) — one conditional top-level
              action next to Regenerate; label and icon reflect the node's
              current is_pinned rather than rendering two items. Same menu
              contract as Regenerate: role=menuitem, roving tabindex,
              close-and-return-focus on activation. */}
          <button
            type="button"
            role="menuitem"
            aria-pressed={node.is_pinned}
            tabIndex={menuActions[activeIndex] === 'pin' ? 0 : -1}
            onClick={() => {
              closeContextMenu();
              toggleNodePinned(node.id).catch((err) => {
                addToast('Pin toggle failed', formatError(err), 'error');
              });
            }}
            title={node.is_pinned ? 'Remove this node from the Pinned grid' : 'Keep this node in the Pinned grid'}
            data-testid="pin-toggle"
            className="w-full text-left px-3 py-1.5 text-xs text-text-secondary hover:bg-bg-card flex items-center gap-2"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill={node.is_pinned ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <path d="M12 17v5" fill="none" />
              <path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
            </svg>
            {node.is_pinned ? 'Unpin node' : 'Pin node'}
          </button>
        </div>,
        document.body,
      )}

      {/* Issue #778 — confirmation dialog for running-node Regenerate.
          Mounted only while a picker click has set `pendingRegenerate`;
          both Confirm and Cancel clear the state, so the Modal's
          window-level Escape listener arms/unarms with the dialog
          itself (no Escape-stealing risk against agent CLIs). */}
      {pendingRegenerate && (
        <ConfirmDialog
          title="Regenerate this node?"
          message={`Agent is currently working. Regenerate with ${pendingRegenerate.providerLabel}?`}
          confirmLabel="Regenerate"
          onConfirm={confirmRegenerate}
          onCancel={cancelRegenerate}
        />
      )}
    </div>
  );
}
