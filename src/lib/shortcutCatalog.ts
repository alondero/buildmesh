/**
 * Keyboard shortcut catalog (issue #731; table-driven since #2240).
 *
 * The single source of truth for the in-app cheatsheet: platform-aware key
 * labels, the action name each binding corresponds to, the human description,
 * and the group the cheatsheet renders the entry under.
 *
 * Two provenances, one list:
 *
 *   - **Window-global actions** (`app` and most of `grid`) are declared in
 *     `shortcutDispatch.ts` — the table that also carries each action's
 *     accelerator, guards and handler — and their catalog rows are derived
 *     from it by `globalShortcutCatalogEntries()`. A binding, the cheatsheet
 *     row that advertises it, and the code that runs it therefore cannot
 *     drift apart, and a test can fire the action instead of grepping
 *     `App.tsx`.
 *   - **Context-bound actions** (terminal zoom / `term-*`, the grid-search
 *     Esc, the Modal's Esc) have no entry in that table: they are bound on a
 *     focused xterm instance or handled by a component's own key handler, not
 *     by the app-level dispatcher. Their rows are declared below.
 *
 * Conventions from `feedback-keyboard-shortcut-conventions.md`:
 * - ⌘ on macOS, Ctrl on Windows/Linux for CommandOrControl+* bindings.
 * - Alt+G on Windows/Linux, ⌘+G on macOS for the grid-toggle (issue #668).
 * - Two modifiers on arrow traversal to avoid stealing readline
 *   `backward-word` / `forward-word`.
 * - Single canonical binding per action — no Ctrl+1..9 / Alt+1..9
 *   parallel paths.
 */
import { globalShortcutCatalogEntries } from './shortcutDispatch';
import { isMac } from './platform';

export type ShortcutGroup = 'app' | 'grid' | 'terminal' | 'modal';

export interface ShortcutEntry {
  /** Stable action id shared with the dispatch table's `action`. */
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
 * Context-bound shortcuts: rows the cheatsheet renders whose handler is NOT
 * the app-level dispatcher. These live on a focused xterm instance
 * (`attachCustomKeyEventHandler` in TerminalRegistry.ts, the zoom keydown
 * listener in Terminal.tsx) or on a component's own key handler (the grid
 * search input's Esc, the &lt;Modal&gt; primitive's Esc), so they have no row
 * in the dispatch table — `dispatchShortcut` reports them as
 * `'unknown-action'`.
 *
 * The window-global rows come first in `SHORTCUT_CATALOG` because they are
 * derived from `globalShortcutCatalogEntries()`, in the table's own order.
 */
const CONTEXT_SHORTCUT_ENTRIES: ReadonlyArray<ShortcutEntry> = [
  {
    // Issue #998 — Esc clears the grid search. Handled contextually by
    // the search input's own `onKeyDown` (it `e.preventDefault()`s the
    // event so the Modal/AgentNodeView Esc listeners don't fire when
    // the input is the active element), so this entry is the
    // discoverability surface only — no App.tsx Tauri global-shortcut
    // registration, no shortcut-triggered dispatch. Listing it in the
    // `grid` group (not `modal`) tells the user this Esc is "for the
    // grid", distinct from the modal group's "close dialog" Esc. Not
    // flagged `splash` for the same reason as `focus-grid-search`
    // above.
    action: 'clear-grid-search',
    group: 'grid',
    description: 'Clear grid search',
    winKey: 'Esc',
    macKey: 'Esc',
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
    // Issue #1264 — the previous `Ctrl++` label was the literal "press
    // Ctrl and the + key" rendering, but the actual US-keyboard chord
    // is `Ctrl+=` (the unshifted key) and the matcher in
    // `terminalKeyAction.ts:60` accepts both `=` and `+` for
    // tolerance. `Ctrl+=` is what the user actually types and matches
    // the convention used for `Ctrl+0` / `Ctrl+-` in the rows above
    // and below, so flip the label to match the real chord.
    winKey: 'Ctrl+=',
    macKey: '⌘+=',
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
    description: 'Clear focused terminal scrollback',
    winKey: 'Ctrl+Shift+L',
    macKey: '⌘+Shift+K',
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
 * Every shortcut shipped in the app: the window-global rows derived from the
 * dispatch table (`shortcutDispatch.ts`), then the context-bound rows above.
 * Pinned by `tests/unit/shortcut-catalog.test.ts` and, for the dispatch
 * half, by `tests/unit/shortcut-catalog-binding.test.ts`.
 *
 * Drift policy (issue #2240): the table rows and the catalog rows are the same
 * objects, so "a binding exists but the cheatsheet omits it" is no longer
 * possible for window-global actions — the parity test pins the two sets as
 * equal in both directions. A context row can still be orphaned (nothing
 * dispatches it); the parity test pins those action ids explicitly so a new
 * orphan has to be declared.
 */
export const SHORTCUT_CATALOG: ReadonlyArray<ShortcutEntry> = [
  ...globalShortcutCatalogEntries(),
  ...CONTEXT_SHORTCUT_ENTRIES,
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
