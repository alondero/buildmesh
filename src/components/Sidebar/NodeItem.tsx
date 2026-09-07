import { useState, useEffect, useLayoutEffect, useMemo, useRef, useCallback } from 'react';
import { createPortal } from 'react-dom';
import type { AgentNode } from '../../stores/agentNodeStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { getStatusConfig } from '../../lib/status';
import { canResumeSuspendedNode, hasLostConversation } from '../../lib/suspended';
import { MissingSessionIdBadge } from '../shared/MissingSessionIdBadge';
import { getMeshColor } from '../../lib/meshColors';
import type { SpawnOption } from '../../lib/groups';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { RegenerateProviderMenu } from '../Providers/RegenerateProviderMenu';
import { InlineEditableText } from '../shared/InlineEditableText';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useRegenerateAction } from '../../hooks/useRegenerateAction';
import { useSubmenu, focusWithoutScroll } from '../../hooks/useSubmenu';
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
// The gate itself, the confirm state machine (#778), and the IPC
// dispatch all live in `useRegenerateAction` (shared with
// `GridNodeHeader`, issue #1502) so the two surfaces stay in lockstep.

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
  const isAutopilot = useAgentNodeStore((s) => s.autopilotStates[node.id] != null);
  const lostConversation = hasLostConversation(node, isAutopilot);
  const renameAgentNode = useAgentNodeStore((s) => s.renameAgentNode);
  const spawnAgent = useAgentNodeStore((s) => s.spawnAgent);
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
  // Boolean mirror of the menu-open signal so the layout effect at L347
  // (and any future boolean-only deps) can key off this primitive
  // instead of the full `contextMenu` object reference — depending on
  // the object would re-run the focus dance whenever any field
  // changed (PR #1635 review feedback).
  const isMenuOpen = contextMenu !== null;
  const menuRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLDivElement>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  // Issue #774 / ticket 03 + #1502 — Regenerate picker submenu state and
  // keyboard contract live in the shared `useSubmenu` hook (same hook
  // drives the header kebab submenu): hover/click opens via `setOpen`,
  // ArrowRight opens-and-focuses via `openViaKeyboard`, ArrowLeft closes,
  // ArrowDown/Up wraps via `step`. The picker itself renders the node's
  // current provider pinned on top (`Current (<label>)`, in-place
  // kick-start) followed by alternates grouped by `group_key`.
  const regen = useRegenerateAction(node, providerList);
  const { isRegenerateDisabled, hasRegenerateTargets } = regen;
  const regenSubmenu = useSubmenu({
    disabled: isRegenerateDisabled,
    itemCount: (providerList ?? []).length,
  });

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

  // Close and return focus to the row that opened the menu. Used by
  // Escape and any menuitem click so the user's focus stays
  // predictable across menu interactions. The `requestAnimationFrame`
  // runs after the unmount so the trigger ref is still attached when
  // the focus() lands. Also tears down submenu state so reopening
  // starts from a known-closed picker.
  const closeContextMenu = () => {
    const trigger = triggerRef.current;
    regenSubmenu.closeSubmenu();
    setContextMenu(null);
    requestAnimationFrame(() => trigger?.focus({ preventScroll: true }));
  };

  // Issue #774 — invoke `regenerate_agent_node` with the chosen
  // provider and close the menu. The disabled gate mirrors the trigger
  // so a programmatic `.click()` can't bypass it; the confirm state
  // machine and IPC dispatch live in `useRegenerateAction`.
  const pickProvider = (providerId: string, providerLabel: string) => {
    if (isRegenerateDisabled || !hasRegenerateTargets) return;
    closeContextMenu();
    regen.pickRegenerateProvider(providerId, providerLabel);
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

  // Mirror the roving index into a ref so the document-level listener
  // does not tear down and re-attach on every keystroke (mirrors
  // `useAriaMenu.ts:38-42`). Submenu state needs no mirror: the shared
  // `useSubmenu` hook exposes stable callbacks whose reads are live
  // (fresh DOM queries per keystroke), so the listener attaches once
  // per menu open with no churn.
  const activeIndexRef = useRef(activeIndex);
  activeIndexRef.current = activeIndex;

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
      const active = document.activeElement;
      const inMenu = menu && active instanceof Node && menu.contains(active);
      const inSubmenu = regenSubmenu.submenuContainsFocus();
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
      // Issue #774 — submenu navigation via the shared `useSubmenu`
      // hook. ArrowRight on a submenu trigger opens the picker and
      // moves focus to the first provider (deterministic post-commit
      // focus, no `queueMicrotask` race); ArrowLeft inside the picker
      // — or on its trigger — closes it and returns focus to the
      // trigger.
      if (e.key === 'ArrowRight' && inMenu && !inSubmenu) {
        if (active?.getAttribute('aria-haspopup') !== 'menu') return;
        e.preventDefault();
        regenSubmenu.openSubmenuViaKeyboard();
        return;
      }
      if (
        e.key === 'ArrowLeft' &&
        (inSubmenu || (inMenu && regenSubmenu.isSubmenuOpen()))
      ) {
        e.preventDefault();
        regenSubmenu.closeSubmenu();
        const menuEl = menuRef.current;
        const trigger = menuEl?.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]');
        if (trigger) focusWithoutScroll(trigger);
        return;
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (inSubmenu) {
          // Shared wrap-around step (unfocused start goes to the
          // first row, never the middle).
          regenSubmenu.stepSubmenuFocus(1);
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
          regenSubmenu.stepSubmenuFocus(-1);
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
    // `closeContextMenu` and `regenSubmenu` are local closures that
    // capture `regenSubmenu` (itself a fresh object each render);
    // depending on either would re-attach the keydown listener on
    // every render. The handler reads the live ref values via
    // `activeIndexRef.current` / `regenSubmenu.submenuContainsFocus()`,
    // so the stale-closure trap doesn't apply here. Issue #1542.
    // eslint-disable-next-line react-hooks/exhaustive-deps -- local closures; handler reads live refs so stale-closure trap doesn't apply.
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

  // Issue #1293 — the parent context menu mounts at the right-click
  // point, which lands directly on the Regenerate row (the first
  // menuitem). Chromium sometimes fires `mouseenter` on the wrapper
  // synchronously on mount, popping the submenu without any real
  // pointer movement — the "occasionally" symptom PR #1290 didn't
  // fully pin. Gate hover-open behind a `pointerover` arm so a quiet
  // mount-time `mouseenter` is ignored; click (`onClick` below) and
  // `ArrowRight` (`openSubmenuViaKeyboard`) stay immediate because
  // they don't go through `mouseenter`. The arm is a ref (not state)
  // because a flip shouldn't re-render — only the submenu's open
  // boolean should drive renders. `pointerover` is the FIRST event
  // in the per-spec enter-the-element sequence (before `mouseenter`),
  // so it reliably arms for real hovers in production AND in
  // @testing-library/user-event — see the wrapper's handler comment
  // below for the dispatch-order detail.
  const submenuArmedRef = useRef(false);
  // Reset on every menu open/close so the next right-click starts
  // from a known-unarmed state. Escape / outside-click close the menu
  // (set `contextMenu` to null) and the effect resets the arm; the
  // next mount starts unarmed, requiring real pointer movement again.
  useEffect(() => {
    if (!contextMenu) submenuArmedRef.current = false;
  }, [contextMenu]);

  // Issue #776 — on open, reset the roving index and move focus to the
  // first menuitem so keyboard nav starts somewhere. `useLayoutEffect`
  // (not `useEffect` + setTimeout) — fires synchronously after commit
  // so subsequent arrow-key presses don't race a deferred focus call.
  useLayoutEffect(() => {
    if (!isMenuOpen) return;
    setActiveIndex(0);
    const parentItems = getParentMenuItems();
    if (parentItems[0]) focusWithoutScroll(parentItems[0]);
  }, [isMenuOpen, getParentMenuItems]);

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
      // Issue #1293 — `hover:brightness-125` applied `filter: brightness(...)`
      // which creates a containing block for `position:fixed` descendants,
      // breaking any overlay mounted inside the row (today: the running-node
      // `ConfirmDialog`; tomorrow: any new overlay). Lighten the mesh-tinted
      // background via CSS variables instead — no `filter`, so the row no
      // longer retargets `position:fixed`. The variables carry two alphas of
      // the same mesh colour (rest + hover) so the "row brightens on hover"
      // visual is preserved without any `filter`.
      style={isActive ? undefined : ({
        '--mesh-bg': `${meshColor.hex}40`,
        '--mesh-bg-hover': `${meshColor.hex}99`,
      } as React.CSSProperties)}
      className={`
        pl-3 pr-1 py-1.5 rounded-md text-sm mb-0.5 flex items-center gap-2 group/node
        ${isClosing ? 'opacity-50 pointer-events-none cursor-default' : 'cursor-pointer'}
        ${isActive ? 'border border-accent-cyan/50' : 'border border-transparent bg-[var(--mesh-bg)] hover:bg-[var(--mesh-bg-hover)]'}
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
      {lostConversation && <MissingSessionIdBadge />}
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
          viewport coordinates. Issue #1293 also fixed the row's
          `hover:brightness-125` (a CSS `filter` that creates a
          containing block for `fixed` descendants) — the row now
          uses CSS-variable hover backgrounds. */}
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
              toggles it (tap/touch parity).

              Issue #1293 — `mouseenter` alone doesn't open the picker
              (Chromium quirk fires it on mount under an existing
              cursor). The wrapper's `onPointerOver` arms
              `submenuArmedRef`; only an armed `mouseenter` opens.
              `pointerover` is the FIRST event in the per-spec
              enter-the-element sequence (before `mouseenter`), so it
              reliably arms for real hovers in production AND in
              @testing-library/user-event (whose dispatch order is
              `pointerover → mouseenter → pointermove → mousemove`).
              When the wrapper mounts under an existing cursor, no
              `pointerover` fires (no boundary was crossed), so the
              arm stays false and a stray mount-time `mouseenter` is
              ignored. Click and `ArrowRight` stay immediate (they
              don't go through `mouseenter`). `mouseleave` disarms
              so a re-entry still requires real movement. */}
          <div
            role="presentation"
            className="relative"
            onPointerOver={() => {
              submenuArmedRef.current = true;
            }}
            onMouseEnter={() => {
              if (submenuArmedRef.current && !isRegenerateDisabled) {
                regenSubmenu.setSubmenuOpen(true);
              }
            }}
            onMouseLeave={() => {
              submenuArmedRef.current = false;
              regenSubmenu.closeSubmenu();
            }}
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
              aria-expanded={regenSubmenu.submenuOpen}
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
                regenSubmenu.setSubmenuOpen(true);
              }}
              title={
                isRegenerateDisabled
                  // Use the raw status name rather than `config.label`
                  // so the tooltip stays lowercase and consistent with the
                  // other machine status names (e.g. "while suspended").
                  ? `Regenerate unavailable while ${node.status}`
                  : !hasRegenerateTargets
                    ? 'No providers are available on this mesh'
                    : regenSubmenu.submenuOpen
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
                screen readers announce "menu: <picker label>". Rows
                render via the shared `RegenerateProviderMenu`
                (current pinned on top for in-place kick-start, then
                alternates grouped by harness) so the sidebar and
                `GridNodeHeader` pickers never drift. */}
            {regenSubmenu.submenuOpen && (
              <div
                ref={regenSubmenu.submenuRef}
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
          State lives in `useRegenerateAction` (shared with the header);
          both Confirm and Cancel clear it, so the Modal's window-level
          Escape listener arms/unarms with the dialog itself (no
          Escape-stealing risk against agent CLIs). */}
      {regen.pendingRegenerate && (
        <ConfirmDialog
          title="Regenerate this node?"
          message={`Agent is currently working. Regenerate with ${regen.pendingRegenerate.providerLabel}?`}
          confirmLabel="Regenerate"
          onConfirm={regen.confirmRegenerate}
          onCancel={regen.cancelRegenerate}
        />
      )}
    </div>
  );
}
