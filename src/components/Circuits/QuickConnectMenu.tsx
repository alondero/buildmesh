/**
 * QuickConnectMenu — drag-to-search node menu (issue #1209).
 *
 * When a quick-connect drag releases over empty canvas, the editor
 * opens this fuzzy-search menu at the drop point. Choosing an entry
 * creates that node pre-wired from the dragged handle.
 */

import { useMemo, useState } from 'react';
import { fuzzyFilterSpecs, categoryAccent, type NodeKindSpec } from './circuitGraphModel';
import { useEscapeKey } from '../../hooks/useEscapeKey';

interface QuickConnectMenuProps {
  /** Where the drag released (canvas flow coordinates). */
  position: { x: number; y: number };
  onSelect: (spec: NodeKindSpec) => void;
  onDismiss: () => void;
}

export function QuickConnectMenu({ position, onSelect, onDismiss }: QuickConnectMenuProps) {
  const [query, setQuery] = useState('');
  const results = useMemo(() => fuzzyFilterSpecs(query.trim()), [query]);
  const first = results[0];

  // Issue #649 — Escape dismisses the menu. Routed through the shared
  // `useEscapeKey` hook so the menu (mounted after the circuit editor's
  // window listener was, but now via the LIFO stack) claims Escape
  // before the editor's close path runs. Replaces the previous
  // `e.stopPropagation()` on the input's element-level onKeyDown —
  // brittle, because the editor's listener was on `window` and
  // `stopPropagation` on a child doesn't stop a window listener.
  useEscapeKey(() => onDismiss());

  return (
    <div
      data-testid="quick-connect-menu"
      className="absolute z-30 w-52 rounded-md border border-border-default bg-bg-overlay shadow-xl p-1"
      style={{ left: position.x + 8, top: position.y - 12 }}
    >
      <input
        autoFocus
        value={query}
        placeholder="Search nodes…"
        aria-label="Search nodes"
        data-testid="quick-connect-input"
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && first) {
            e.stopPropagation();
            onSelect(first);
          }
        }}
        className="w-full mb-1 px-1.5 py-0.5 bg-bg-input border border-border-subtle rounded-sm text-xs text-text-primary focus:outline-none"
      />
      <ul className="max-h-48 overflow-y-auto">
        {results.map((spec) => {
          const accent = categoryAccent(spec.category);
          return (
            <li key={spec.discriminator}>
              <button
                type="button"
                data-testid={`quick-connect-${spec.discriminator}`}
                onClick={() => onSelect(spec)}
                className={`w-full text-left px-1.5 py-0.5 rounded-sm text-xs ${accent.text} hover:bg-bg-card-hover`}
              >
                {spec.label}
              </button>
            </li>
          );
        })}
        {results.length === 0 && <li className="px-1.5 py-1 text-2xs text-text-muted">No matches</li>}
      </ul>
    </div>
  );
}
