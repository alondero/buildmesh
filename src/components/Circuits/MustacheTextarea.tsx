/**
 * MustacheTextarea — a textarea with `{{` autocomplete (issue #1209).
 *
 * Typing `{{` opens a chip menu of circuit-context paths
 * (`issue.*`, `pr.*`, `node.*`, `verification.*`, `retry.*`,
 * `circuit.*`). Picking a chip replaces the typed braces with a
 * complete `{{ path }}`. The menu filters as the user keeps typing;
 * Escape or blur dismisses it.
 */

import { useMemo, useRef, useState } from 'react';
import { MUSTACHE_PATHS, insertMustache, fuzzyScore } from './circuitGraphModel';

interface MustacheTextareaProps {
  value: string;
  onChange: (value: string) => void;
  rows?: number;
  placeholder?: string;
  ariaLabel?: string;
  testId?: string;
}

/** Text after a `{{` opener that still counts as autocomplete context. */
function openContext(text: string, caret: number): string | null {
  const before = text.slice(0, caret);
  const open = before.lastIndexOf('{{');
  if (open === -1) return null;
  const context = before.slice(open + 2);
  // A closed brace or stray whitespace line means the user moved on.
  if (/^[a-zA-Z0-9._]*$/.test(context)) return context;
  return null;
}

export function MustacheTextarea({
  value,
  onChange,
  rows = 3,
  placeholder,
  ariaLabel,
  testId,
}: MustacheTextareaProps) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const [context, setContext] = useState<string | null>(null);
  const [caret, setCaret] = useState(0);

  const suggestions = useMemo(() => {
    if (context === null) return [];
    const q = context.trim();
    const scored = MUSTACHE_PATHS.map((p) => ({ p, s: fuzzyScore(q, p) ?? -Infinity })).filter(
      (x) => x.s > -Infinity
    );
    scored.sort((a, b) => b.s - a.s || a.p.localeCompare(b.p));
    return scored.map((x) => x.p);
  }, [context]);

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
          setTimeout(() => setContext(null), 150);
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
          className="absolute z-30 left-0 right-0 mt-1 max-h-40 overflow-y-auto bg-bg-overlay border border-border-default rounded-md shadow-lg py-1"
        >
          {suggestions.map((path) => (
            <li key={path}>
              <button
                type="button"
                data-testid={`mustache-chip-${path}`}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pick(path)}
                className="w-full text-left px-2 py-1 text-xs font-mono text-text-secondary hover:bg-accent-cyan/15 hover:text-accent-cyan"
              >
                {path}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
