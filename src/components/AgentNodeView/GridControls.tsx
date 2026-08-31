import { useEffect, useRef } from 'react';

import { useUIStore } from '../../stores/uiStore';
import { registerGridSearchInput } from '../../lib/gridSearchFocus';

/**
 * The grid search control (issue #998).
 *
 * This is the *minimum* UI #998 needs in order for the keyboard shortcuts
 * to have a target: a controlled text input bound to
 * `uiStore.gridSearchQuery`, with a clear button and an Esc handler.
 * The filter popover, sort selector, direction toggle, active-filter
 * badges, and global reset button called out in ticket #997 ("Build View
 * Header Filter & Sort UI components") are intentionally not here —
 * that ticket is the natural home for them, and stacking them on this
 * PR would conflate the work and balloon the diff.
 *
 * Visibility: the search input lives in the right-hand area of the
 * grid, mounted only when there is at least one view mode that renders
 * the grid (i.e. NOT in `single` mode, where the soloed node fills the
 * canvas and a search input has nothing to filter). The single-mode
 * check is intentionally a render-time guard rather than a
 * `viewMode === 'single'` branch on the input itself, so the search
 * input doesn't briefly appear then vanish on the Single-mode toggle.
 *
 * Esc handling: handled inline in the input's `onKeyDown` (not in
 * App.tsx's window keydown listener), so it only fires when the input
 * itself has focus. `e.stopPropagation()` prevents the Event from
 * bubbling to the modal/Single-mode Esc handlers — the user is
 * clearing a search, not closing a dialog.
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

  const inputRef = useRef<HTMLInputElement>(null);

  // Register the input with the module-level focus singleton so the
  // `focus-grid-search` shortcut (Ctrl+F / ⌘+⌥+F, wired in App.tsx) can
  // call `.focus()` on it from outside the component tree. A layout
  // effect runs synchronously after the DOM mutation and before the
  // browser paints, so the first ⌘+F from a cold load can't land before
  // the ref is set.
  useEffect(() => {
    registerGridSearchInput(inputRef.current);
    return () => {
      // Clear the singleton on unmount so a future remount (mesh
      // change, view-mode flip) doesn't leave a stale detached node
      // that `.focus()` would silently target.
      registerGridSearchInput(null);
    };
  }, []);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Escape') {
      // Stop the event from bubbling to the Modal/AutoNodeView Esc
      // listeners. The user is clearing the search, not closing a
      // dialog or exiting a solo view — Esc inside the search input
      // means "wipe what I typed", full stop.
      e.preventDefault();
      e.stopPropagation();
      if (query !== '') {
        setQuery('');
      }
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
        className="bg-bg-card border border-border-default rounded-md px-2 py-1 text-xs text-text-primary placeholder:text-text-muted w-44 outline-none focus:border-accent-cyan focus:ring-1 focus:ring-accent-cyan/30 transition-colors"
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
