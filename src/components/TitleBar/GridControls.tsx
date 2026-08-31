import { useLayoutEffect, useRef } from 'react';

import { useUIStore } from '../../stores/uiStore';

/**
 * The grid search control (issue #998).
 *
 * Lives in the slim top View Header — the same right-hand strip as the
 * ViewModeSwitcher. This is the *minimum* UI #998 needs in order for the
 * keyboard shortcuts to have a target: a controlled text input bound to
 * `uiStore.gridSearchQuery`, with a clear button, an Esc handler, and a
 * subscription to the `focus-grid-search` request counter. The filter
 * popover, sort selector, direction toggle, active-filter badges, and
 * global reset button called out in ticket #997 ("Build View Header
 * Filter & Sort UI components") are intentionally not here — that ticket
 * is the natural home for them, and stacking them on this PR would
 * conflate the work and balloon the diff.
 *
 * ## Focus request flow
 *
 * The App.tsx `focus-grid-search` Tauri global-shortcut handler does NOT
 * call any imperative API on this component. Instead it bumps
 * `useUIStore.focusGridSearchRequest`, a monotonically increasing counter.
 * This component subscribes to that counter in a `useLayoutEffect` and
 * calls `.focus()` + `.select()` on the rendered input every time the
 * counter changes. The counter pattern is the React-idiomatic "imperative
 * command from outside the component tree" channel: no ref forwarding,
 * no module-level singleton, no test-only `__resetXyzForTests` seam.
 * Two ⌘+F presses bump the counter 0 → 1 → 2, the effect fires twice,
 * the input is focused and re-selected on each press.
 *
 * `useLayoutEffect` (not `useEffect`) so the focus moves before the
 * browser paints — the user shouldn't see a single frame of "I pressed
 * Ctrl+F but the input didn't focus yet". Same timing reason other
 * focus-management code in this codebase uses the layout flavour.
 *
 * ## Esc handling
 *
 * Handled inline in the input's `onKeyDown` (not in App.tsx's window
 * keydown listener), so it only fires when the input itself has focus —
 * the `close-modal` window-level Esc still closes dialogs when the user
 * is not focused on the search. The behaviour matches the universal
 * search-bar pattern:
 *
 *   - query !== '' → first Esc clears the text (user can immediately
 *     type a new search; staying focused is the helpful default).
 *   - query === '' → Esc blurs the input and returns focus to the
 *     canvas, so the user can keep using the existing xterm / arrow
 *     shortcuts.
 *
 * `e.stopPropagation()` is called in both branches so the Esc never
 * bubbles to the modal / Single-mode Esc listeners — the user is
 * clearing a search or leaving it, not closing a dialog.
 *
 * ## Native search-cancel button
 *
 * `<input type="search">` renders a native ✕ in WebKit (macOS Tauri) and
 * Blink (Windows Tauri). Without the Tailwind arbitrary-variant CSS
 * `[&::-webkit-search-cancel-button]:appearance-none` the user would see
 * a stacked double-✕ (the native one plus our custom button below).
 * The `type="search"` is kept for the accessibility win (screen readers
 * announce the field as a search input) — we just hide the native UI.
 */
export function GridControls() {
  // Read the query directly — the selector returns a primitive, so
  // Zustand's default equality (`Object.is`) is the right behaviour:
  // unrelated `uiStore` field changes (a probe tab switch, a cheatsheet
  // toggle) don't re-render the input. The setter is stable across
  // renders (Zustand keeps a single fn reference), so reading it via a
  // second selector avoids the stale-closure trap of destructuring
  // `useUIStore.getState()` at component scope.
  const query = useUIStore((s) => s.gridSearchQuery);
  const setQuery = useUIStore((s) => s.setGridSearchQuery);
  // The monotonic focus request counter. Bumped by App.tsx on every
  // `focus-grid-search` shortcut press. We do NOT compare against a
  // stale closure of the previous value — the effect's dep array is
  // `[request]`, so the effect re-fires on every distinct bump.
  const focusRequest = useUIStore((s) => s.focusGridSearchRequest);

  const inputRef = useRef<HTMLInputElement>(null);

  // React to every focus request. `useLayoutEffect` so the focus moves
  // before paint (no flash of un-focused input). The `focusRequest > 0`
  // guard covers the initial mount with the default counter (0): we
  // don't want to grab focus on first render — the user just opened
  // the app and almost certainly isn't asking to type into the search
  // box. The cold-load case where `focusRequest > 0` on first mount
  // (the user pressed ⌘+F in the brief window between Tauri-start
  // and TitleBar-mount) IS handled: the effect runs on mount with the
  // current value, and `focusRequest > 0` is true, so the input
  // focuses.
  useLayoutEffect(() => {
    if (focusRequest === 0) return;
    const el = inputRef.current;
    if (el === null) return;
    el.focus();
    // Universal "find" behaviour: select the existing text so the next
    // keystroke replaces it. Without `.select()`, the user lands at the
    // end of the field (WebKit) or the start (Blink) and has to
    // backspace manually to clear the previous search. The spec calls
    // this out explicitly: review feedback on PR #1448.
    el.select();
  }, [focusRequest]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key !== 'Escape') return;
    // Stop the event from bubbling to the modal / Single-mode Esc
    // listeners in BOTH branches — the user is interacting with the
    // search input, not closing a dialog or exiting a solo view.
    e.preventDefault();
    e.stopPropagation();
    if (query !== '') {
      // First Esc: clear the text. The user stays focused so they can
      // immediately type a new search; if they want to leave the
      // field, a second Esc (when the query is now '') blurs (see the
      // else branch). This matches GitHub / Slack / Linear search
      // behaviour.
      setQuery('');
    } else {
      // Second Esc (or first Esc on an empty input): blur, returning
      // focus to the canvas so the user can keep using xterm / arrow
      // shortcuts. Without this, focus was trapped inside the input
      // and the user had to click out manually — review feedback on
      // PR #1448.
      e.currentTarget.blur();
    }
  };

  return (
    <div
      className="flex items-center gap-1"
      data-testid="grid-controls"
    >
      <input
        ref={inputRef}
        type="search"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder="Search nodes…"
        aria-label="Search nodes"
        data-testid="grid-search-input"
        // The `[&::-webkit-search-cancel-button]:appearance-none`
        // arbitrary variant hides the native ✕ that WebKit / Blink
        // render on `<input type="search">`. Without it the user sees
        // a double-✕ (native + our custom button below). Keeping
        // `type="search"` is intentional — it lets screen readers
        // announce the field as a search input. The custom button
        // (rendered conditionally below) is the only visible clear
        // affordance.
        className="bg-bg-card border border-border-default rounded-md px-2 py-1 text-xs text-text-primary placeholder:text-text-muted w-44 outline-none focus:border-accent-cyan focus:ring-1 focus:ring-accent-cyan/30 transition-colors [&::-webkit-search-cancel-button]:appearance-none"
      />
      {query !== '' && (
        <button
          type="button"
          onClick={() => setQuery('')}
          data-testid="grid-search-clear"
          aria-label="Clear search"
          title="Clear search (Esc)"
          className="text-text-muted hover:text-status-error text-xs px-1 transition-colors"
        >
          ✕
        </button>
      )}
    </div>
  );
}
