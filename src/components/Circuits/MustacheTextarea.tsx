/**
 * MustacheTextarea — a textarea with `{{` autocomplete (issue #1209).
 *
 * Typing `{{` opens a chip menu of circuit-context paths
 * (`issue.*`, `pr.*`, `node.*`, `verification.*`, `retry.*`,
 * `circuit.*`). Picking a chip replaces the typed braces with a
 * complete `{{ path }}`. The menu filters as the user keeps typing;
 * Escape or blur dismisses it. Arrow keys + Enter navigate without
 * leaving the keyboard — a click-only menu is an obstacle, not
 * autocomplete (issue #1359 review feedback).
 *
 * Issue #1359: when the parent supplies a `reachable` reachability
 * summary, suggestions are grouped by namespace (Issue Context, Pull
 * Request, Node Outputs, …) and unreachable chips are dimmed but
 * remain selectable — the user may still want to author a prompt that
 * resolves empty in this branch (e.g. a draft for an issue-label branch
 * before the trigger is wired up).
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import {
  MUSTACHE_GROUPS,
  MUSTACHE_PATHS,
  fuzzyScore,
  groupForPath,
  insertMustache,
  isReachablePath,
  type ReachableContext,
} from './circuitGraphModel';

interface MustacheTextareaProps {
  value: string;
  onChange: (value: string) => void;
  rows?: number;
  placeholder?: string;
  ariaLabel?: string;
  testId?: string;
  /** Pre-computed reachability for the node whose template this is.
   *  Parent computes it (via `getReachableContext`) so the same summary
   *  can feed the inspector's context-reference drawer without
   *  recomputing on every keystroke. Omitting falls back to the
   *  ungrouped, all-live menu used by ad-hoc fields outside the canvas. */
  reachable?: ReachableContext;
}

/** Text after a `{{` opener that still counts as autocomplete context.
 *  Spaces are allowed — `{{ ` is exactly what insertMustache produces
 *  around the path, so typing one must not close the menu. Hyphens,
 *  slashes, and colons are also allowed so node ids like `spawn-1`
 *  and branch references like `feat/circuits` keep the menu open
 *  mid-typing. */
function openContext(text: string, caret: number): string | null {
  const before = text.slice(0, caret);
  const open = before.lastIndexOf('{{');
  if (open === -1) return null;
  const context = before.slice(open + 2);
  // A closed brace or stray punctuation means the user moved on.
  if (/^[a-zA-Z0-9._:/\- ]*$/.test(context)) return context;
  return null;
}

export function MustacheTextarea({
  value,
  onChange,
  rows = 3,
  placeholder,
  ariaLabel,
  testId,
  reachable,
}: MustacheTextareaProps) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const blurTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [context, setContext] = useState<string | null>(null);
  const [caret, setCaret] = useState(0);
  // Index of the highlighted chip in `suggestions`. `-1` means no
  // selection (the menu opens unselected, like every OS autocomplete).
  const [highlight, setHighlight] = useState(-1);

  // Don't let a pending blur-close fire after unmount.
  useEffect(() => {
    return () => {
      if (blurTimer.current !== null) clearTimeout(blurTimer.current);
    };
  }, []);

  // Flat list filtered by the user's prefix. Spawn-output chips
  // (`node.<id>.output`) are minted dynamically from
  // `reachable.nodeOutputIds` so the popup reflects the actual graph,
  // not a hand-curated catalogue.
  const suggestions = useMemo(() => {
    if (context === null) return [];
    const q = context.trim();
    const dynamic: string[] = [];
    if (reachable !== undefined) {
      for (const id of reachable.nodeOutputIds) {
        dynamic.push(`node.${id}.output`);
      }
    }
    const catalogue = [...MUSTACHE_PATHS, ...dynamic];
    const scored = catalogue
      .map((p) => ({ p, s: fuzzyScore(q, p) ?? -Infinity }))
      .filter((x) => x.s > -Infinity);
    scored.sort((a, b) => b.s - a.s || a.p.localeCompare(b.p));
    return scored.map((x) => x.p);
  }, [context, reachable]);

  // Grouped view: when reachability is available, render headers above
  // each namespace bucket. Order mirrors `MUSTACHE_GROUPS` so the
  // popup and the inspector drawer agree on which bucket comes first.
  const grouped = useMemo(() => {
    if (reachable === undefined) return null;
    const buckets = new Map<string, string[]>();
    for (const spec of MUSTACHE_GROUPS) buckets.set(spec.namespace, []);
    for (const path of suggestions) {
      const ns = groupForPath(path);
      const list = buckets.get(ns);
      if (list !== undefined) list.push(path);
    }
    const result: Array<{ spec: (typeof MUSTACHE_GROUPS)[number]; paths: string[] }> = [];
    for (const spec of MUSTACHE_GROUPS) {
      const paths = buckets.get(spec.namespace) ?? [];
      if (paths.length > 0) result.push({ spec, paths });
    }
    return result;
  }, [reachable, suggestions]);

  // Flat list of paths in render order so keyboard navigation can
  // step through every visible chip regardless of grouping. The
  // highlight index is always an offset into `flatPaths` — NOT into
  // `suggestions`, which is sorted by fuzzy score and disagrees with
  // the DOM order once grouping kicks in (review feedback #1359
  // round 2: pressing Enter on a chip highlighted via ArrowDown
  // must insert the highlighted chip, not the raw fuzzy top).
  const flatPaths = useMemo(() => {
    if (grouped === null) return suggestions;
    return grouped.flatMap((g) => g.paths);
  }, [grouped, suggestions]);

  // Clamp the highlight into the new render-order range whenever the
  // list shrinks or the menu reopens — otherwise ArrowDown could
  // escape the end and Enter would `pick(undefined)`.
  useEffect(() => {
    if (flatPaths.length === 0) {
      if (highlight !== -1) setHighlight(-1);
      return;
    }
    if (highlight >= flatPaths.length) setHighlight(flatPaths.length - 1);
  }, [flatPaths.length, highlight]);

  const handleChange = (next: string) => {
    onChange(next);
    const pos = ref.current?.selectionStart ?? next.length;
    setCaret(pos);
    setContext(openContext(next, pos));
    // Reset highlight whenever the prefix changes — the user is
    // mid-typing and the previous selection no points to a different
    // chip (or no chip at all).
    setHighlight(-1);
  };

  const pick = (path: string) => {
    const result = insertMustache(value, caret, path);
    onChange(result.text);
    setContext(null);
    setHighlight(-1);
    // Restore focus/caret so typing continues right after the insertion.
    requestAnimationFrame(() => {
      ref.current?.focus();
      ref.current?.setSelectionRange(result.caret, result.caret);
    });
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (context === null) return;
    switch (e.key) {
      case 'Escape':
        e.stopPropagation();
        setContext(null);
        setHighlight(-1);
        return;
      case 'ArrowDown': {
        e.preventDefault();
        if (flatPaths.length === 0) return;
        setHighlight((h) => (h + 1 >= flatPaths.length ? 0 : h + 1));
        return;
      }
      case 'ArrowUp': {
        e.preventDefault();
        if (flatPaths.length === 0) return;
        setHighlight((h) => (h <= 0 ? flatPaths.length - 1 : h - 1));
        return;
      }
      case 'Enter':
      case 'Tab': {
        // Pick the highlighted chip, or the top chip if none
        // highlighted. Always index into `flatPaths` — the render
        // order — so the picked chip matches the highlighted one in
        // the DOM (issue #1359 review round 2).
        if (flatPaths.length === 0) return;
        const idx = highlight >= 0 ? highlight : 0;
        e.preventDefault();
        pick(flatPaths[idx]);
        return;
      }
      default:
        return;
    }
  };

  return (
    <div className="relative">
      <textarea
        ref={ref}
        value={value}
        rows={rows}
        placeholder={placeholder}
        aria-label={ariaLabel}
        aria-autocomplete="list"
        aria-controls="mustache-menu"
        data-testid={testId}
        onChange={(e) => handleChange(e.target.value)}
        onBlur={() => {
          // Let chip clicks land before the menu disappears.
          if (blurTimer.current !== null) clearTimeout(blurTimer.current);
          blurTimer.current = setTimeout(() => {
            setContext(null);
            setHighlight(-1);
          }, 150);
        }}
        onKeyDown={onKeyDown}
        className="w-full px-2 py-1 bg-bg-input border border-border-subtle rounded-md text-text-primary font-mono text-xs resize-none focus:outline-none focus:border-border-active"
      />
      {suggestions.length > 0 && (
        <ul
          id="mustache-menu"
          role="listbox"
          data-testid="mustache-menu"
          data-grouped={grouped !== null ? 'true' : 'false'}
          className="absolute z-30 left-0 right-0 mt-1 max-h-56 overflow-y-auto bg-bg-overlay border border-border-default rounded-md shadow-lg py-1"
        >
          {grouped === null ? (
            // Ungrouped fallback (legacy callers without a graph).
            suggestions.map((path, idx) => (
              <li key={path} role="presentation">
                <button
                  type="button"
                  role="option"
                  aria-selected={highlight === idx}
                  data-testid={`mustache-chip-${path}`}
                  data-reachable="true"
                  data-highlighted={highlight === idx ? 'true' : 'false'}
                  onMouseDown={(e) => e.preventDefault()}
                  onMouseEnter={() => setHighlight(idx)}
                  onClick={() => pick(path)}
                  className={`w-full text-left px-2 py-1 text-xs font-mono ${
                    highlight === idx
                      ? 'bg-accent-cyan/25 text-accent-cyan'
                      : 'text-text-secondary hover:bg-accent-cyan/15 hover:text-accent-cyan'
                  }`}
                >
                  {path}
                </button>
              </li>
            ))
          ) : (
            grouped.map(({ spec, paths }) => (
              <li key={spec.namespace} className="border-b border-border-subtle/40 last:border-b-0" role="presentation">
                <div
                  className="px-2 pt-1.5 pb-0.5 text-2xs uppercase tracking-wide text-text-muted"
                  data-testid={`mustache-group-${spec.namespace}`}
                >
                  {spec.label}
                </div>
                <ul role="group">
                  {paths.map((path) => {
                    const live = isReachablePath(path, reachable);
                    // Compute the flat index for keyboard highlight.
                    const flatIdx = flatPaths.indexOf(path);
                    return (
                      <li key={path} role="presentation">
                        <button
                          type="button"
                          role="option"
                          aria-selected={highlight === flatIdx}
                          data-testid={`mustache-chip-${path}`}
                          data-reachable={live ? 'true' : 'false'}
                          data-highlighted={highlight === flatIdx ? 'true' : 'false'}
                          onMouseDown={(e) => e.preventDefault()}
                          onMouseEnter={() => setHighlight(flatIdx)}
                          onClick={() => pick(path)}
                          title={
                            live
                              ? spec.description
                              : `${spec.description} — not reachable in this branch`
                          }
                          className={`w-full flex items-center justify-between gap-2 px-2 py-1 text-xs font-mono text-left ${
                            highlight === flatIdx
                              ? live
                                ? 'bg-accent-cyan/25 text-accent-cyan'
                                : 'bg-bg-card text-status-warning'
                              : live
                              ? 'text-text-secondary hover:bg-accent-cyan/15 hover:text-accent-cyan'
                              : 'text-text-muted/60 hover:bg-bg-card'
                          }`}
                        >
                          <span>{path}</span>
                          {!live && (
                            <span
                              data-testid={`mustache-chip-${path}-unreachable`}
                              aria-label="unreachable in this branch"
                              className="text-2xs uppercase tracking-wide text-status-warning/80"
                            >
                              empty
                            </span>
                          )}
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </li>
            ))
          )}
        </ul>
      )}
    </div>
  );
}