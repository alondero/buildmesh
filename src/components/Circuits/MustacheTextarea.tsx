/**
 * MustacheTextarea — a textarea with `{{` autocomplete (issue #1209).
 *
 * Typing `{{` opens a chip menu of circuit-context paths
 * (`issue.*`, `pr.*`, `node.*`, `verification.*`, `retry.*`,
 * `circuit.*`). Picking a chip replaces the typed braces with a
 * complete `{{ path }}`. The menu filters as the user keeps typing;
 * Escape or blur dismisses it.
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
 *  around the path, so typing one must not close the menu. */
function openContext(text: string, caret: number): string | null {
  const before = text.slice(0, caret);
  const open = before.lastIndexOf('{{');
  if (open === -1) return null;
  const context = before.slice(open + 2);
  // A closed brace or stray punctuation means the user moved on.
  if (/^[a-zA-Z0-9._ ]*$/.test(context)) return context;
  return null;
}

/** True when a chip is "live" — a producer upstream guarantees the
 *  variable resolves to a non-empty value when this template fires.
 *  `circuit.*` and `node.id` are always live (the trigger wrapper sets
 *  them at run creation). `node.<id>.output` lives iff `id` is in
 *  `reachable.nodeOutputIds`. Everything else maps directly to the
 *  trigger / gate booleans on the reachability summary. */
function isReachable(
  path: string,
  reachable: ReachableContext | undefined
): boolean {
  if (reachable === undefined) return true;
  const ns = groupForPath(path);
  switch (ns) {
    case 'circuit':
      // circuit.* is always populated by the trigger wrapper.
      return true;
    case 'node':
      // `node.id` is always live (with_node); `node.<id>.output` is
      // keyed by the spawn id, checked via the suffix match below.
      if (path === 'node.id') return true;
      if (path.endsWith('.output')) {
        // Reconstruct the spawn id (path is `node.<id>.output`).
        const id = path.slice('node.'.length, -'.output'.length);
        return reachable.nodeOutputIds.includes(id);
      }
      return false;
    case 'issue':
      return reachable.triggers.issue;
    case 'pr':
      return reachable.pullRequest;
    case 'verification':
      return reachable.gates.verification;
    case 'retry':
      return reachable.gates.retry;
    case 'spawn_output':
      // Defensive — `groupForPath` already routes `.output` chips into
      // the spawn_output namespace, but we still test the suffix
      // because someone could call `isReachable` with a raw chip.
      if (path.endsWith('.output')) {
        const id = path.slice('node.'.length, -'.output'.length);
        return reachable.nodeOutputIds.includes(id);
      }
      return false;
    default:
      return false;
  }
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

  // Don't let a pending blur-close fire after unmount.
  useEffect(() => {
    return () => {
      if (blurTimer.current !== null) clearTimeout(blurTimer.current);
    };
  }, []);

  // When the parent pre-computes reachability (the canvas inspector
  // knows the selected node id and passes `reachable`), the popup is
  // grouped and unreachable chips are dimmed. Without a node id (the
  // ad-hoc reuse case outside the canvas), skip reachability so the
  // legacy flat menu keeps working.

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

  const handleChange = (next: string) => {
    onChange(next);
    const pos = ref.current?.selectionStart ?? next.length;
    setCaret(pos);
    setContext(openContext(next, pos));
  };

  const pick = (path: string) => {
    const result = insertMustache(value, caret, path);
    onChange(result.text);
    setContext(null);
    // Restore focus/caret so typing continues right after the insertion.
    requestAnimationFrame(() => {
      ref.current?.focus();
      ref.current?.setSelectionRange(result.caret, result.caret);
    });
  };

  return (
    <div className="relative">
      <textarea
        ref={ref}
        value={value}
        rows={rows}
        placeholder={placeholder}
        aria-label={ariaLabel}
        data-testid={testId}
        onChange={(e) => handleChange(e.target.value)}
        onBlur={() => {
          // Let chip clicks land before the menu disappears.
          if (blurTimer.current !== null) clearTimeout(blurTimer.current);
          blurTimer.current = setTimeout(() => setContext(null), 150);
        }}
        onKeyDown={(e) => {
          if (e.key === 'Escape' && context !== null) {
            e.stopPropagation();
            setContext(null);
          }
        }}
        className="w-full px-2 py-1 bg-bg-input border border-border-subtle rounded-md text-text-primary font-mono text-xs resize-none focus:outline-none focus:border-border-active"
      />
      {suggestions.length > 0 && (
        <ul
          data-testid="mustache-menu"
          data-grouped={grouped !== null ? 'true' : 'false'}
          className="absolute z-30 left-0 right-0 mt-1 max-h-56 overflow-y-auto bg-bg-overlay border border-border-default rounded-md shadow-lg py-1"
        >
          {grouped === null ? (
            // Ungrouped fallback (legacy callers without a graph).
            suggestions.map((path) => (
              <li key={path}>
                <button
                  type="button"
                  data-testid={`mustache-chip-${path}`}
                  data-reachable="true"
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => pick(path)}
                  className="w-full text-left px-2 py-1 text-xs font-mono text-text-secondary hover:bg-accent-cyan/15 hover:text-accent-cyan"
                >
                  {path}
                </button>
              </li>
            ))
          ) : (
            grouped.map(({ spec, paths }) => (
              <li key={spec.namespace} className="border-b border-border-subtle/40 last:border-b-0">
                <div
                  className="px-2 pt-1.5 pb-0.5 text-2xs uppercase tracking-wide text-text-muted"
                  data-testid={`mustache-group-${spec.namespace}`}
                >
                  {spec.label}
                </div>
                <ul>
                  {paths.map((path) => {
                    const live = isReachable(path, reachable);
                    return (
                      <li key={path}>
                        <button
                          type="button"
                          data-testid={`mustache-chip-${path}`}
                          data-reachable={live ? 'true' : 'false'}
                          onMouseDown={(e) => e.preventDefault()}
                          onClick={() => pick(path)}
                          title={
                            live
                              ? spec.description
                              : `${spec.description} — not reachable in this branch`
                          }
                          className={`w-full flex items-center justify-between gap-2 px-2 py-1 text-xs font-mono text-left ${
                            live
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