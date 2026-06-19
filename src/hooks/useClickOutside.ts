/**
 * useClickOutside — close a scoped dropdown on outside mousedown.
 *
 * Issue #492: the "close dropdown on outside click" effect was duplicated
 * across four files (`GitIssuesTab`, `GitPullRequestsTab`,
 * `SessionHistoryTab`, `Sidebar`), each installing a document-level
 * `mousedown` listener that closed the dropdown when the event target
 * did not match a `[data-dropdown-for="<id>"]` ancestor. `Sidebar.tsx`
 * had drifted to a loose selector (`[data-dropdown-for]` with no value),
 * which would have closed one sidebar dropdown when the user clicked a
 * *different* dropdown's body — a latent bug that this hook fixes as a
 * side effect of the extraction by pinning the scoped variant.
 *
 * Shape
 * -----
 * `useClickOutside(open, onClose, attributeName?)` attaches a single
 * `mousedown` listener on `document` while `open !== null`. The listener
 * closes (calls `onClose`) iff the click target is NOT inside the element
 * carrying the scoped attribute. The selector is always
 * `[<attributeName>="<open>"]` — there is no "loose" mode.
 *
 * `open` doubles as both the "should we listen" gate AND the selector
 * value. Pass the same value the dropdown container tags itself with
 * (e.g. `openDropdown = pr.number` ↔ `data-dropdown-for={pr.number}`).
 *
 * `onClose` is read through a ref so callers do not need to memoize —
 * the listener is attached once per `open` flip, not on every render.
 * `attributeName` is read fresh inside the effect (it's included in the
 * effect's deps); callers who need a stable attribute name should pass
 * a literal or a `useMemo`'d value. All current callers use the default.
 *
 * Out of scope
 * ------------
 * `BuildRunDropdown.tsx` (ref.contains shape) and `MeshItem.tsx` /
 * `Terminal.tsx` (context-menu close + Escape key) use the same
 * `mousedown` skeleton but differ in how they decide "inside". They
 * were intentionally left out of #492's scope; a future hook variant
 * could absorb them if the duplication recurs.
 *
 * Usage
 * -----
 *     const [open, setOpen] = useState<number | null>(null);
 *     useClickOutside(open, () => setOpen(null));
 *
 *     // In JSX: tag the open container with the same value.
 *     {open === issue.number && (
 *       <div data-dropdown-for={issue.number} ...>...</div>
 *     )}
 */
import { useEffect, useRef } from 'react';

export function useClickOutside<T>(
  open: T | null,
  onClose: () => void,
  attributeName: string = 'data-dropdown-for',
): void {
  // `onClose` is read through a ref so the listener attachment is keyed
  // only on `open` (and `attributeName`). A fresh `() => setOpen(null)`
  // closure each render would otherwise tear down + rebuild the document
  // listener on every parent re-render. `attributeName` is a static config
  // value in practice (all current callers use the default) and is
  // included in the deps for correctness if a caller ever passes a
  // dynamic value.
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (open === null) return;
    const selector = `[${attributeName}="${String(open)}"]`;
    const handler = (event: MouseEvent) => {
      const target = event.target as HTMLElement | null;
      if (target?.closest(selector)) return;
      onCloseRef.current();
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [open, attributeName]);
}