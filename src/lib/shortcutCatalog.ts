/**
 * Keyboard shortcut catalog (issue #731).
 *
 * The single source of truth for the in-app cheatsheet. App.tsx owns the
 * Tauri-binding side (which fires even when an xterm terminal has focus
 * and so can't be served by a plain window keydown listener); this module
 * owns the *display* side: platform-aware key labels, the action name
 * each binding corresponds to, the human description, and the group the
 * cheatsheet renders the entry under.
 *
 * Why not colocate the binding list with the catalog? Because the two
 * layers have different concerns: the binding list is Tauri accelerator
 * strings (`CommandOrControl+T` — one canonical key per action), the
 * catalog is *display* labels (`⌘+T` on macOS, `Ctrl+T` elsewhere). A
 * shared `action` id is the join key; keeping the wiring and the
 * metadata in their natural homes avoids over-fitting a "single source
 * of truth" past its useful shape.
 *
 * Conventions from `feedback-keyboard-shortcut-conventions.md`:
 * - ⌘ on macOS, Ctrl on Windows/Linux for CommandOrControl+* bindings.
 * - Alt+G on Windows/Linux, ⌘+G on macOS for the grid-toggle (issue #668).
 * - Two modifiers on arrow traversal to avoid stealing readline
 *   `backward-word` / `forward-word`.
 * - Single canonical binding per action — no Ctrl+1..9 / Alt+1..9
 *   parallel paths.
 */
import { isMac } from './platform';

export type ShortcutGroup = 'app' | 'grid' | 'terminal' | 'modal';

export interface ShortcutEntry {
  /** Stable action id shared with App.tsx's Tauri-binding side. */
  action: string;
  /** Group for the cheatsheet's section heading. */
  group: ShortcutGroup;
  /** One-line human description of what the shortcut does. */
  description: string;
  /** Windows/Linux key label, e.g. "Ctrl+T" or "Alt+G" or "Ctrl+Alt+←". */
  winKey: string;
  /** macOS key label, e.g. "⌘+T" or "⌘+G" or "⌘+⌥+←". */
  macKey: string;
  /**
   * True if this entry should also be surfaced in the empty-state splash
   * (AgentNodeView's "no mesh loaded" discoverability hint). Issue #748:
   * the splash used to hand-code five kbd rows that duplicated catalog
   * entries, so a future rename (e.g. `?` → `Ctrl+/`) would silently
   * drift. Marking entries with `splash: true` makes the catalog the
   * single source of truth for both surfaces. The catalog test pins
   * which entries are splash so a refactor that drops one is caught.
   */
  splash?: boolean;
}

/**
 * Display order for the cheatsheet's group sections. Curated to match
 * the user's mental flow: window-global first (the bindings that work
 * everywhere), then grid (the second-most-common surface), then terminal
 * (xterm-only), then modal (escape hatch).
 */
const GROUP_ORDER: ShortcutGroup[] = ['app', 'grid', 'terminal', 'modal'];

/**
 * Display name for each group. Keep these short — they sit as section
 * headings in a ~max-w-md modal panel.
 */
const GROUP_TITLES: Record<ShortcutGroup, string> = {
  app: 'App',
  grid: 'Grid',
  terminal: 'Terminal',
  modal: 'Modal',
};

/**
 * Every shortcut shipped in the app. Pinned by `tests/unit/shortcut-catalog.test.ts`.
 *
 * `action` ids are *stable React keys for the catalog itself*, not a
 * contract with App.tsx dispatch or with the terminal key resolvers.
 * The bindings live in three different places (Tauri global-shortcut
 * registrations in App.tsx, the terminal's `attachCustomKeyEventHandler`
 * chain in TerminalRegistry.ts, and the window-level keydown listener in
 * App.tsx) — each binding has its own native identifier. The catalog's
 * `action` id is the join key the cheatsheet uses to render the row; it
 * does NOT need to match any of those identifiers.
 *
 * Drift policy:
 *   - Adding a binding in App.tsx / TerminalRegistry.ts without adding a
 *     catalog entry: the cheatsheet will silently omit it. Reviewable
 *     by code review; no automated guard.
 *   - Adding a catalog entry without a live binding: the cheatsheet will
 *     advertise a key that does nothing. The catalog test pins the
 *     required entries by name (the *reverse* direction), so this is
 *     caught at test time.
 */
export const SHORTCUT_CATALOG: ReadonlyArray<ShortcutEntry> = [
  // --- App (window-global, Tauri global-shortcut plugin) -----------------
  {
    action: 'new-agent',
    group: 'app',
    description: 'New agent node',
    winKey: 'Ctrl+T',
    macKey: '⌘+T',
    splash: true,
  },
  {
    action: 'open-cheatsheet',
    group: 'app',
    description: 'Show this cheatsheet',
    winKey: '?',
    macKey: '?',
    splash: true,
  },
  {
    // Issue #64: jump to the next agent node with `status === 'awaiting_input'`.
    // Surfaced in the splash (not just the cheatsheet) because the typical use
    // case is "I just noticed a node needs my input while I'm focused on a
    // terminal prompt" — the splash hint teaches the gesture before the user
    // ever opens the help modal. `Ctrl+.` is the conventional "next" chord
    // (matches `Cmd+.` for "next issue" in many editors).
    action: 'jump-to-next-awaiting',
    group: 'app',
    description: 'Jump to next node awaiting input',
    winKey: 'Ctrl+.',
    macKey: '⌘+.',
    splash: true,
  },

  // --- Grid (window-global) -----------------------------------------------
  {
    action: 'toggle-maximize-grid',
    group: 'grid',
    description: 'Toggle grid / single view',
    winKey: 'Alt+G',
    macKey: '⌘+G',
    splash: true,
  },
  {
    action: 'arrow-left',
    group: 'grid',
    description: 'Move focus left (wrap within row)',
    winKey: 'Ctrl+Alt+←',
    macKey: '⌘+⌥+←',
    splash: true,
  },
  {
    action: 'arrow-right',
    group: 'grid',
    description: 'Move focus right (wrap within row)',
    winKey: 'Ctrl+Alt+→',
    macKey: '⌘+⌥+→',
    splash: true,
  },
  {
    action: 'arrow-up',
    group: 'grid',
    description: 'Move focus up (no-op at top edge)',
    winKey: 'Ctrl+Alt+↑',
    macKey: '⌘+⌥+↑',
    splash: true,
  },
  {
    action: 'arrow-down',
    group: 'grid',
    description: 'Move focus down (no-op at bottom edge)',
    winKey: 'Ctrl+Alt+↓',
    macKey: '⌘+⌥+↓',
    splash: true,
  },

  // --- Terminal (window-level or xterm-only) ------------------------------
  // The zoom keys are window-level (Terminal.tsx keydown listener).
  // The action keys are xterm-only (TerminalRegistry.attachCustomKeyEventHandler).
  {
    action: 'zoom-reset',
    group: 'terminal',
    description: 'Reset terminal font size',
    winKey: 'Ctrl+0',
    macKey: '⌘+0',
  },
  {
    action: 'zoom-in',
    group: 'terminal',
    description: 'Increase terminal font size',
    winKey: 'Ctrl++',
    macKey: '⌘++',
  },
  {
    action: 'zoom-out',
    group: 'terminal',
    description: 'Decrease terminal font size',
    winKey: 'Ctrl+-',
    macKey: '⌘+-',
  },
  {
    action: 'term-copy',
    group: 'terminal',
    description: 'Copy selection from focused terminal',
    winKey: 'Ctrl+Shift+C',
    macKey: '⌘+C',
  },
  {
    action: 'term-paste',
    group: 'terminal',
    description: 'Paste into focused terminal',
    winKey: 'Ctrl+Shift+V',
    macKey: '⌘+V',
  },
  {
    action: 'term-select-all',
    group: 'terminal',
    description: 'Select all in focused terminal',
    winKey: 'Ctrl+Shift+A',
    macKey: '⌘+A',
  },
  {
    action: 'term-find',
    group: 'terminal',
    description: 'Find in focused terminal',
    winKey: 'Ctrl+Shift+F',
    macKey: '⌘+F',
  },
  {
    action: 'term-clear',
    group: 'terminal',
    description: 'Clear focused terminal',
    winKey: 'Ctrl+Shift+K',
    macKey: '⌘+K',
  },

  // --- Modal (context-bound Escape) ---------------------------------------
  // Escape is owned by the <Modal> primitive — its window keydown listener
  // arms only while a modal is mounted, so it doesn't steal Escape from
  // agent CLIs in the grid. The same key also un-maximizes the grid (in
  // AgentNodeView's effect), so the description spans both surfaces.
  {
    action: 'close-modal',
    group: 'modal',
    description: 'Close current dialog or exit maximized view',
    winKey: 'Esc',
    macKey: 'Esc',
    splash: true,
  },
];

/**
 * Pick the platform-appropriate label for a catalog entry. The cheatsheet
 * calls this per row so the displayed keys always match the OS the user
 * is on — same convention as Terminal.tsx and GridNodeHeader.tsx
 * (isMac ? '⌘' : 'Ctrl').
 */
export function shortcutLabel(entry: ShortcutEntry, mac = isMac): string {
  return mac ? entry.macKey : entry.winKey;
}

export interface GroupedShortcuts {
  group: ShortcutGroup;
  title: string;
  entries: ShortcutEntry[];
}

/**
 * Bucket the catalog by group, in display order, dropping any group that
 * has no entries. The cheatsheet renders one section per bucket.
 *
 * Implementation note (issue #748): single-pass reduce over the catalog
 * rather than the previous `GROUP_ORDER.map(g => catalog.filter(...))`
 * pattern. With the 16 shipped shortcuts the difference is invisible
 * (~68 comparisons vs. ~16), but this helper sets the pattern for future
 * `groupByX` helpers — `O(entries)` instead of `O(groups × entries)` keeps
 * the helper correct when the catalog grows past ~50 entries. Empty
 * groups are simply absent from the buckets object; we walk `GROUP_ORDER`
 * at the end to preserve display order.
 */
export function groupedShortcutEntries(
  catalog: ReadonlyArray<ShortcutEntry> = SHORTCUT_CATALOG,
): GroupedShortcuts[] {
  const buckets = catalog.reduce<Partial<Record<ShortcutGroup, ShortcutEntry[]>>>(
    (acc, entry) => {
      (acc[entry.group] ??= []).push(entry);
      return acc;
    },
    {},
  );
  return GROUP_ORDER
    .map(group => {
      const entries = buckets[group];
      if (!entries || entries.length === 0) return null;
      return { group, title: GROUP_TITLES[group], entries };
    })
    .filter((g): g is GroupedShortcuts => g !== null);
}
