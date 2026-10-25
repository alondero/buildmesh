import { useAgentNodeStore } from '../stores/agentNodeStore';
import { useNodeActivityStore } from '../stores/nodeActivityStore';
import { useUIStore } from '../stores/uiStore';
import { jumpToNextAwaitingNode } from './awaitingInputShortcuts';
import { isTextInputFocused } from './focusGuard';
import {
  buildFocusGridSearchBinding,
  cycleGridMode,
  toggleGridMaximize,
  triggerNewAgentShortcut,
} from './gridShortcuts';
import { traversalTargetId } from './gridTraversal';
import { createKeyRepeatThrottle } from './keyRepeatThrottle';
import { focusNode } from './navigation';
import { activityRootId, groupActivityNodes, indexAgentNodes } from './nodeActivities';
import { createShortcutGuard } from './shortcutGuard';
import { readScope } from '../hooks/useScope';
import type { GlobalShortcutBinding } from '../hooks/useGlobalShortcuts';
import type { ShortcutEntry } from './shortcutCatalog';

/**
 * Table-driven global-shortcut dispatch (issue #2240).
 *
 * One row per shortcut action, carrying everything a shortcut needs to be
 * wired, guarded, displayed and tested:
 *
 *   - `accelerator` — the Tauri global-shortcut binding, branched on platform
 *     (`null` for the one gesture App reaches from a window keydown listener
 *     instead, because the `?` key has no single accelerator shape).
 *   - `guards` — the focus gate and the rate limiter (block-all cooldown vs.
 *     leading-edge throttle), declared as data so the dispatcher applies the
 *     focus check *before* the limiter. That ordering is load-bearing: a
 *     limiter records the press the moment it is consulted, so a
 *     focus-blocked press must never reach it or it would silently swallow
 *     the user's next deliberate press inside the window.
 *   - `run` — the effect.
 *   - `catalog` — the cheatsheet row. `shortcutCatalog.ts` derives its
 *     window-global entries from this table, so a binding, its guards and its
 *     discoverability surface can no longer drift apart.
 *
 * Why this exists: the wiring used to be a 170-line `if (action === ...)`
 * chain in `App.tsx` whose only test coverage was regexing that file's text,
 * so an action's id had to agree in three places (binding, branch, catalog)
 * and nothing but a substring match checked it. Tests now fire
 * `dispatchShortcut('some-action')` and assert the effect.
 */
export interface ShortcutActionSpec {
  /** Stable action id — the join key with `SHORTCUT_CATALOG`. */
  readonly action: string;
  /**
   * Tauri accelerator for this action, branched on platform. `null` when the
   * action is NOT an OS-level global shortcut (the `?` cheatsheet toggle,
   * which stays a window keydown listener — see that entry's comment).
   */
  readonly accelerator: ((mac: boolean) => string) | null;
  readonly guards?: {
    /**
     * Predicate returning true when the press must be dropped because a text
     * input holds focus. `isTextInputFocused` carves out the xterm helper
     * textarea, so a terminal prompt does NOT block the action.
     */
    readonly focus?: () => boolean;
    readonly rate?: RateLimitSpec;
  };
  readonly run: () => void | Promise<void>;
  /** Cheatsheet metadata; the catalog's window-global entries come from here. */
  readonly catalog: Omit<ShortcutEntry, 'action'>;
}

/**
 * Rate limiter for one action.
 *
 * `cooldown` blocks every press inside the window (right for one-shot
 * toggles like Alt+G); `throttle` lets the first press through and rate-limits
 * the rest (right for grid traversal, where holding the arrow key SHOULD
 * repeat — just slower). They are different primitives on purpose: wrapping
 * the arrows in a cooldown would silently turn traversal into a one-shot
 * move.
 *
 * `key` is the limiter's identity and defaults to the action id. Actions that
 * must share one limiter — the four arrow directions — name the same key, so
 * holding Left does not starve Right.
 */
export interface RateLimitSpec {
  readonly kind: 'cooldown' | 'throttle';
  readonly ms: number;
  readonly key?: string;
}

/** What a dispatched action actually did. */
export type ShortcutDispatch =
  /** The handler ran. */
  | 'handled'
  /** A text input held focus, so the press was dropped before the limiter. */
  | 'blocked-focus'
  /** The cooldown had not elapsed; the press was dropped. */
  | 'blocked-cooldown'
  /** The throttle had not elapsed; the press was dropped. */
  | 'blocked-throttle'
  /** No table entry claims this action id (terminal/modal/context gestures). */
  | 'unknown-action';

type RateGate = (run: () => void | Promise<void>) => boolean;

/**
 * Run a handler, swallowing a rejection. `createShortcutGuard` already catches
 * what it awaits, but an async row dispatched without a limiter (or through
 * the throttle) would otherwise surface as an unhandled rejection.
 */
function invoke(run: () => void | Promise<void>): void {
  const result = run();
  if (result) void result.catch(() => {});
}

// 300ms cooldown for the one-shot toggles (issue #668's Alt+G budget, shared by
// the other toggles): long enough that holding the key doesn't fire a burst,
// short enough that a deliberate second press feels responsive.
const COOLDOWN_MS = 300;
// Leading-edge throttle for arrow traversal: 200ms = ~5 moves/sec on a held
// key. Snappier than the cooldowns because navigation benefits from
// responsiveness, while still comfortably catching the OS autorepeat flood
// (~30 Hz Windows, ~15 Hz macOS).
const ARROW_THROTTLE_MS = 200;

// Limiter instances, keyed by `${kind}:${key}`. Module scope on purpose: a
// guard created inside the App render body would reset on every remount, which
// is exactly the "burst on hold" bug the limiters exist to prevent.
const rateGates = new Map<string, RateGate>();

function rateGateFor(action: string, rate: RateLimitSpec): RateGate {
  const key = `${rate.kind}:${rate.key ?? action}`;
  const existing = rateGates.get(key);
  if (existing) return existing;
  let gate: RateGate;
  if (rate.kind === 'cooldown') {
    const guard = createShortcutGuard(rate.ms);
    gate = run => {
      // `createShortcutGuard` reports only "ran / didn't run", so recover the
      // outcome from the fact that `action()` is invoked synchronously inside
      // the guard: `admitted` is already true when `run` returns.
      let admitted = false;
      guard(async () => {
        admitted = true;
        await run();
      });
      return admitted;
    };
  } else {
    const throttle = createKeyRepeatThrottle(rate.ms);
    gate = run => {
      if (!throttle()) return false;
      invoke(run);
      return true;
    };
  }
  rateGates.set(key, gate);
  return gate;
}

/**
 * Move focus one cell across the on-screen grid (Ctrl/Cmd+Alt+Arrow).
 *
 * Arrow traversal works in two phases: in Single view mode the first arrow
 * press exits the solo view back onto the node the user was viewing
 * (`activeNodeId` already points at it); on the next press (and beyond) we
 * walk the on-screen grid of the active View Mode from that node. Edge
 * semantics are defined in `arrowTargetIndex` (src/lib/gridTraversal.ts):
 * Left/Right wrap within the row, Up/Down are no-ops at the grid's vertical
 * edges.
 *
 * Note: this differs from the Escape-to-exit-Single path in AgentNodeView.tsx
 * (which leaves activeNodeId alone). Esc is a passive exit; Ctrl+Arrow is an
 * exit-and-move, so it makes sense to refocus the node the user was on.
 */
function traverseGrid(direction: ArrowDirection): void {
  // Phase 1: in Single view mode (which subsumes the old maximize wayfinder
  // #982), the first arrow press restores the grid AND keeps focus on the
  // soloed node. Single renders the active node, so exiting the mode is all
  // that's needed — activeNodeId already points at the node being viewed.
  const ui = useUIStore.getState();
  if (ui.viewMode === 'single') {
    ui.exitSingleMode();
    return;
  }

  // Phase 2: walk the grid the active View Mode actually renders. Ticket
  // #987 — `readScope` returns exactly the on-screen node set (Mesh for the
  // selected mesh, Pinned cross-mesh, All, or Filtered narrowed by the Grid
  // Controls — #1609) in the store's canonical (mesh_id, position) order, so
  // traversal matches AgentNodeView cell-for-cell. This replaces the old
  // `mesh_id === activeNode.mesh_id` filter, which stranded Ctrl+Alt+Arrow
  // on the active node's mesh in Pinned/All. The scope comes from the same
  // assembly the grid renders, and with the Mesh fallback chain gone, a Mesh
  // Grid with no selection walks an empty set (there is nothing on screen to
  // walk). Focusing within the current grid preserves its mesh scope.
  const activeNode = useAgentNodeStore.getState().getActiveNode();
  if (!activeNode) return;
  // Issue #1384 — `getAgentNodes()` is the store's derived getter; it allocates
  // a fresh ordered array from `nodesById` + `nodeIds` on each call. Cheaper
  // than reading the old (now-removed) `agentNodes` field, and matches the
  // imperative-reader pattern used by `awaitingInputShortcuts` /
  // `meshStore.deleteMesh`.
  const agentNodes = useAgentNodeStore.getState().getAgentNodes();
  const ownerships = useAgentNodeStore.getState().circuitOwnerships;
  const nodeIndex = indexAgentNodes(agentNodes);
  const groups = useNodeActivityStore.getState().groups;
  const scope = readScope();
  const visibleNodes = groupActivityNodes(scope.visibleNodes, nodeIndex, ownerships, groups);
  const targetId = traversalTargetId(visibleNodes, activityRootId(activeNode.id, nodeIndex, ownerships, groups), direction);
  if (targetId !== null) {
    focusNode(targetId);
  }
}

/**
 * Every window-global shortcut in the app, in cheatsheet display order.
 *
 * Array order IS the `app` / `grid` section order of the cheatsheet, because
 * `shortcutCatalog.ts` builds its window-global entries from this list; the
 * context-bound rows (terminal, modal, grid-search Esc) live there.
 */
export const SHORTCUT_ACTIONS: readonly ShortcutActionSpec[] = [
  {
    action: 'new-agent',
    accelerator: () => 'CommandOrControl+T',
    guards: { rate: { kind: 'cooldown', ms: COOLDOWN_MS } },
    // Async on purpose: the spawn resolves the mesh's default provider over
    // IPC. The cooldown guard awaits it, so a rejection lands in the guard's
    // catch rather than becoming an unhandled rejection.
    run: triggerNewAgentShortcut,
    catalog: {
      group: 'app',
      description: 'New agent node',
      winKey: 'Ctrl+T',
      macKey: '⌘+T',
      splash: true,
    },
  },
  {
    // Issue #731 — the `?` cheatsheet toggle is a plain window keydown
    // listener, NOT a Tauri global-shortcut registration, for two reasons:
    //   1. `?` is shifted on US layouts and unmodified on some European
    //      layouts, so an accelerator string would tie us to one key-shape per
    //      platform and force two registrations. A keydown listener matches
    //      `e.key` semantically, which is what the user means.
    //   2. A focused xterm terminal sees the keystroke first — terminals are
    //      the one place the user might type `?` as a literal character.
    // So `accelerator` is null and App dispatches this action from its keydown
    // listener; the guards below still run here. The `e.repeat` and
    // `isTerminalFocused` checks stay in App because they are keydown-shape
    // guards, not action guards:
    //   - `e.repeat`: a held `?` autorepeats at ~30 Hz after ~500 ms on
    //     Windows; without this guard the modal flickers open/closed for as
    //     long as the user keeps the key down.
    //   - `isTerminalFocused()`: typing `?` in an agent terminal is the user
    //     asking the agent a question — the `?` is content, not a help
    //     request. This is the inverse of the `isTextInputFocused` xterm
    //     carve-out used by Alt+G (which DOES want to fire from a terminal
    //     prompt).
    action: 'open-cheatsheet',
    accelerator: null,
    guards: {
      focus: isTextInputFocused,
      rate: { kind: 'cooldown', ms: COOLDOWN_MS },
    },
    run: () => { useUIStore.getState().toggleCheatsheet(); },
    catalog: {
      group: 'app',
      description: 'Show this cheatsheet',
      winKey: '?',
      macKey: '?',
      splash: true,
    },
  },
  {
    // Issue #64 — Ctrl/Cmd+. cycles to the next agent node whose
    // `status === 'awaiting_input'` in the active node's mesh, wrapping at the
    // end. Registered as a Tauri global shortcut (not a window keydown
    // listener) so the binding fires while an xterm terminal has focus — the
    // typical state when the user notices a node waiting for input and wants
    // to jump to it without clicking. `Period` is Tauri's alias for the `.`
    // key, and `CommandOrControl` resolves to Cmd on macOS / Ctrl elsewhere,
    // so the one string works cross-platform. Surfaced in the splash (not
    // just the cheatsheet) because the typical use case is "I just noticed a
    // node needs my input while I'm focused on a terminal prompt".
    //
    // No focus guard: the user is most likely focused on an xterm prompt (the
    // reason they're noticing the agent needs input), and `isTextInputFocused`
    // carves out the xterm-helper-textarea so this fires from a terminal
    // prompt — exactly what we want. No rate limit either: rapid presses are
    // a legitimate "skip through" gesture, and the cycle is purely
    // synchronous so there's no IPC round-trip to rate-limit.
    action: 'jump-to-next-awaiting',
    accelerator: () => 'CommandOrControl+Period',
    run: () => { jumpToNextAwaitingNode(); },
    catalog: {
      group: 'app',
      description: 'Jump to next node awaiting input',
      winKey: 'Ctrl+.',
      macKey: '⌘+.',
      splash: true,
    },
  },
  {
    // Issue #1409 — the Universal Command Omnibar. The chord MUST fire while
    // an xterm terminal has focus, so it lives here as a Tauri global shortcut
    // (same reason as Ctrl+T). But bare CommandOrControl+K / +P would steal
    // readline's Ctrl+K (kill-line) and Ctrl+P (previous-history) from every
    // Win/Linux shell — the same war the Alt+Grid-toggle and Ctrl/Cmd+Alt+Arrow
    // bindings already fought. So like the grid toggle, we branch the binding
    // on platform: Cmd+K / Cmd+P on macOS (meta is free), and Ctrl+Shift+K /
    // Ctrl+Shift+P elsewhere (Shift-augmented, no readline collision).
    //
    // The K/P distinction is carried in the action so the palette can
    // preselect its mode; `toggleOmnibar` gives the universal "activation
    // chord closes the open modal" behaviour.
    action: 'open-omnibar',
    accelerator: mac => (mac ? 'CommandOrControl+K' : 'CommandOrControl+Shift+K'),
    run: () => { useUIStore.getState().toggleOmnibar('files'); },
    catalog: {
      group: 'app',
      description: 'Open the universal command bar',
      winKey: 'Ctrl+Shift+K',
      macKey: '⌘+K',
      splash: true,
    },
  },
  {
    // Same chord family as `open-omnibar`, in command mode (editors' "quick
    // open file / run command" convention). Not flagged `splash`: the splash
    // is the pre-spawn empty-state hint, where "command" mode has nothing to
    // run yet — same discipline as `cycle-grid-modes`.
    action: 'open-omnibar-commands',
    accelerator: mac => (mac ? 'CommandOrControl+P' : 'CommandOrControl+Shift+P'),
    run: () => { useUIStore.getState().toggleOmnibar('commands'); },
    catalog: {
      group: 'app',
      description: 'Open the universal command bar (commands)',
      winKey: 'Ctrl+Shift+P',
      macKey: '⌘+P',
    },
  },
  {
    // Issue #668 — Alt+G (Win/Linux) / Cmd+G (macOS) toggles grid ↔ single
    // view. The toggle logic (restore if maximized, else maximize the active
    // node) is a pure store mutator in `src/lib/gridShortcuts.ts`; this row
    // owns the platform wiring, the cooldown so a held key doesn't
    // burst-toggle, and the text-input focus guard so typing an inline-renamed
    // node header or a future search box doesn't accidentally collapse the
    // grid.
    //
    // Tauri 2's global-shortcut plugin doesn't expose an `AltOrCommand`
    // combinator (only `CommandOrControl`, which means Cmd on Mac and Ctrl
    // elsewhere), and issue #668 explicitly wants the mnemonic "G for Grid" on
    // Windows/Linux, so we register the platform-appropriate binding only.
    action: 'toggle-maximize-grid',
    accelerator: mac => (mac ? 'CommandOrControl+G' : 'Alt+G'),
    guards: {
      focus: isTextInputFocused,
      rate: { kind: 'cooldown', ms: COOLDOWN_MS },
    },
    run: () => { toggleGridMaximize(); },
    catalog: {
      group: 'grid',
      description: 'Toggle grid / single view',
      winKey: 'Alt+G',
      macKey: '⌘+G',
      splash: true,
    },
  },
  {
    // Ticket #987 — Ctrl+Alt+G / Cmd+Alt+G rotates the grid View Modes; the
    // rotation itself is the pure `cycleGridMode` mutator. Platform-uniform
    // `CommandOrControl+Alt+G`: it can't collide with the Single solo toggle
    // (`Alt+G` on Win/Linux, `Cmd+G` on macOS) because it carries the extra
    // Alt/⌥ modifier. Same guard discipline as Alt+G: focus guard so the
    // canvas doesn't rotate behind an inline rename, cooldown so a held key
    // can't burst-cycle through Mesh → Pinned → All faster than the user can
    // read the switcher.
    action: 'cycle-grid-modes',
    accelerator: () => 'CommandOrControl+Alt+G',
    guards: {
      focus: isTextInputFocused,
      rate: { kind: 'cooldown', ms: COOLDOWN_MS },
    },
    run: () => { cycleGridMode(); },
    catalog: {
      group: 'grid',
      description: 'Cycle grid modes (Mesh → Pinned → All)',
      winKey: 'Ctrl+Alt+G',
      macKey: '⌘+⌥+G',
    },
  },
  // --- Arrow traversal ----------------------------------------------------
  // These bindings carry TWO modifiers (not bare Ctrl/Cmd+Arrow) because
  // Ctrl+←/→ is the readline `backward-word` / `forward-word` gesture in
  // bash/zsh/fish/PSReadLine and every node/python REPL. The global-shortcut
  // plugin captures at the OS layer (before xterm sees the keydown), so bare
  // Ctrl+Arrow would silently steal word-movement whenever the user tries to
  // fix a typo in a long agent prompt. Adding `Alt+` matches the Windows Snap
  // / tmux prefix+Arrow / i3 / VS Code "Move Editor Group" precedent for pane
  // navigation and collides with nothing.
  //
  // The four directions share ONE throttle (`key: 'arrow'`): the OS autorepeat
  // flood is a property of the key being held, not of the direction, so a
  // separate limiter per direction would let holding Left reset the budget and
  // starve Right.
  {
    action: 'arrow-left',
    accelerator: () => 'CommandOrControl+Alt+ArrowLeft',
    guards: arrowGuards(),
    run: () => traverseGrid('left'),
    catalog: {
      group: 'grid',
      description: 'Move focus left (wrap within row)',
      winKey: 'Ctrl+Alt+←',
      macKey: '⌘+⌥+←',
      splash: true,
    },
  },
  {
    action: 'arrow-right',
    accelerator: () => 'CommandOrControl+Alt+ArrowRight',
    guards: arrowGuards(),
    run: () => traverseGrid('right'),
    catalog: {
      group: 'grid',
      description: 'Move focus right (wrap within row)',
      winKey: 'Ctrl+Alt+→',
      macKey: '⌘+⌥+→',
      splash: true,
    },
  },
  {
    action: 'arrow-up',
    accelerator: () => 'CommandOrControl+Alt+ArrowUp',
    guards: arrowGuards(),
    run: () => traverseGrid('up'),
    catalog: {
      group: 'grid',
      description: 'Move focus up (no-op at top edge)',
      winKey: 'Ctrl+Alt+↑',
      macKey: '⌘+⌥+↑',
      splash: true,
    },
  },
  {
    action: 'arrow-down',
    accelerator: () => 'CommandOrControl+Alt+ArrowDown',
    guards: arrowGuards(),
    run: () => traverseGrid('down'),
    catalog: {
      group: 'grid',
      description: 'Move focus down (no-op at bottom edge)',
      winKey: 'Ctrl+Alt+↓',
      macKey: '⌘+⌥+↓',
      splash: true,
    },
  },
  {
    // Issue #998 — Ctrl+F (Win/Linux) / ⌘+⌥+F (macOS) focuses the grid search
    // input. The platform branch and the macOS collision carve-out live in
    // `buildFocusGridSearchBinding` so they can be unit-tested directly.
    //
    // Since #1609 the search lives in the Filtered view (its controls are
    // hidden in other modes), so the shortcut first lands the canvas there —
    // matching every editor's "find in files" gesture — then bumps the
    // request counter. The actual `.focus()` + `.select()` calls live in
    // `GridControls`' `useLayoutEffect` (which subscribes to
    // `useUIStore.focusGridSearchRequest`). A request counter is the
    // React-idiomatic channel for "imperative command from outside the
    // component tree" — no ref forwarding, no module-level singleton, no leaky
    // DOM registration. Ordering is guaranteed by React, not by defensive
    // no-ops: `enterFilteredFromSearch` flips the mode and bumps the counter in
    // one store action, so React re-renders ONCE (18 batching) with both
    // values — TitleBar mounts GridControls, the input ref attaches during
    // commit, and GridControls' `useLayoutEffect` runs after the DOM mutation
    // but before paint, reading the already-bumped counter. There is no
    // intermediate committed frame in which the input is absent, so no press
    // can be dropped; no "input not mounted yet" state exists to defend
    // against.
    //
    // No rate limit: a held key must not burn a window — the user
    // re-pressing while already focused is a harmless re-focus, and the
    // counter pattern (0 → 1 → 2) naturally fires the effect on every
    // distinct press.
    //
    // #2076 — the flip, the focus request and the "these results span Meshes"
    // notice are one store action, shared with the Filtered segment. The
    // notice is informational and runs after the mode flip and the focus bump,
    // so it can never suppress either.
    action: 'focus-grid-search',
    // `buildFocusGridSearchBinding` returns a whole binding object because it
    // predates the table (issue #998); the table only needs the accelerator.
    accelerator: mac => buildFocusGridSearchBinding(mac).key,
    run: () => { useUIStore.getState().enterFilteredFromSearch(); },
    catalog: {
      group: 'grid',
      description: 'Focus grid search',
      winKey: 'Ctrl+F',
      macKey: '⌘+⌥+F',
    },
  },
];

type ArrowDirection = 'left' | 'right' | 'up' | 'down';

/**
 * The guard set shared by all four arrow rows: a defensive focus guard and the
 * shared leading-edge throttle. The focus guard matches the Alt+G row — the
 * binding is Ctrl/Cmd+Alt+Arrow, which has no readline collision, but if the
 * user is focused on a real text input (an inline rename, a future search box)
 * we'd rather not move the grid behind their typing. The
 * xterm-helper-textarea carve-out in `isTextInputFocused()` makes it a no-op
 * when an agent terminal has focus — exactly what we want.
 *
 * A THROTTLE (not a cooldown) is the right limiter here: the user DOES want a
 * held arrow to repeat, just slower. The throttle sits BELOW the focus guard
 * in `dispatchShortcut` — a held arrow inside a rename box must not consume
 * the budget, or the user's next real traversal press (after they tab out of
 * the input) would also be blocked.
 */
function arrowGuards(): NonNullable<ShortcutActionSpec['guards']> {
  return {
    focus: isTextInputFocused,
    rate: { kind: 'throttle', ms: ARROW_THROTTLE_MS, key: 'arrow' },
  };
}

const SPECS_BY_ACTION = new Map(SHORTCUT_ACTIONS.map(spec => [spec.action, spec]));

/**
 * Every Tauri binding to register, for the platform the app is running on.
 * `App.tsx` hands this straight to `useGlobalShortcuts`.
 */
export function shortcutBindings(mac: boolean): GlobalShortcutBinding[] {
  const bindings: GlobalShortcutBinding[] = [];
  for (const spec of SHORTCUT_ACTIONS) {
    if (!spec.accelerator) continue;
    bindings.push({ key: spec.accelerator(mac), action: spec.action });
  }
  return bindings;
}

/**
 * Run the action's handler and report what happened.
 *
 * Every caller gets the same three-part answer — did the focus gate block it,
 * did the rate limiter block it, or did the handler run — which is what lets
 * tests assert behaviour instead of reading source. A keydown caller that
 * needs to know whether to `preventDefault()` can branch on `'blocked-focus'`
 * rather than re-implementing the guard.
 */
export function dispatchShortcut(action: string): ShortcutDispatch {
  const spec = SPECS_BY_ACTION.get(action);
  if (!spec) return 'unknown-action';
  if (spec.guards?.focus?.()) return 'blocked-focus';
  const rate = spec.guards?.rate;
  if (!rate) {
    invoke(spec.run);
    return 'handled';
  }
  if (!rateGateFor(action, rate)(spec.run)) {
    return rate.kind === 'cooldown' ? 'blocked-cooldown' : 'blocked-throttle';
  }
  return 'handled';
}

/** The cheatsheet rows for the window-global actions, in catalog order. */
export function globalShortcutCatalogEntries(): ShortcutEntry[] {
  return SHORTCUT_ACTIONS.map(spec => ({ action: spec.action, ...spec.catalog }));
}

/**
 * @internal — drops every memoised limiter so a test starts with no cooldown
 * and no throttle spent. The limiters are deliberately process-wide (they must
 * survive an App remount), which means a test that fires a burst of presses
 * would otherwise leave the next case's first press swallowed. Mirrors
 * `_resetGlobalShortcutsQueueForTests` in `hooks/useGlobalShortcuts.ts`.
 */
export function _resetShortcutRateGatesForTests(): void {
  rateGates.clear();
}