import { useLayoutEffect, useRef, useState } from 'react';

interface InlineEditableTextProps {
  value: string;
  onCommit: (next: string) => void | Promise<void>;
  className?: string;
  inputClassName?: string;
  style?: React.CSSProperties;
  maxLength?: number;
  ariaLabel?: string;
  /// Optional DOM id for the display-state span. Mirrors MeshItem's
  /// `mesh-item-name-${id}` pattern so a host row's context-menu
  /// `aria-labelledby` points at a name-only element (not the whole
  /// row, whose textContent includes icons + inline actions + the
  /// menu itself). Only applied in display state — the input swaps
  /// its own accessible name from `ariaLabel`, and a stale id would
  /// mis-label the edit field.
  id?: string;
}

/**
 * Renders `value` as text, switching to a same-width input on double-click
 * (mouse) or Enter / Space (keyboard). Enter commits, Escape cancels,
 * blur commits a non-empty value (cancels if blank). Width, font, and
 * colour are inherited from `className` so the swap is visually seamless
 * inside a `truncate` flex parent.
 *
 * Why a span and not a button: the host rows (sidebar NodeItem,
 * GridNodeHeader, etc.) all have their own click handlers that activate
 * the node. Wrapping the name in a real <button> would steal the click
 * (need stopPropagation) and confuse screen readers. role="button" +
 * tabIndex=0 + onKeyDown gives the same keyboard ergonomics without the
 * click-collision.
 */
export function InlineEditableText({
  value,
  onCommit,
  className,
  inputClassName,
  style,
  maxLength = 80,
  ariaLabel = 'Rename',
  id,
}: InlineEditableTextProps) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const inputRef = useRef<HTMLInputElement>(null);
  // Tracks the value at edit start so Escape can restore it without
  // re-deriving from `value` (which may have been updated by a backend
  // event mid-edit).
  const valueAtEditStartRef = useRef(value);

  useLayoutEffect(() => {
    if (editing) {
      const input = inputRef.current;
      if (input) {
        input.focus();
        input.select();
      }
    } else {
      // External value change (LLM rename, another window) — keep draft
      // in sync so the next edit starts from the latest name.
      setDraft(value);
    }
  }, [editing, value]);

  const enterEdit = () => {
    valueAtEditStartRef.current = value;
    setDraft(value);
    setEditing(true);
  };

  const cancel = () => {
    setDraft(valueAtEditStartRef.current);
    setEditing(false);
  };

  const commit = () => {
    const next = draft.trim();
    if (next === '' || next === valueAtEditStartRef.current) {
      cancel();
      return;
    }
    setEditing(false);
    void onCommit(next);
  };

  if (editing) {
    return (
      <input
        ref={inputRef}
        type="text"
        value={draft}
        maxLength={maxLength}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            commit();
          } else if (e.key === 'Escape') {
            e.preventDefault();
            cancel();
          }
        }}
        onBlur={commit}
        onClick={(e) => e.stopPropagation()}
        onDoubleClick={(e) => e.stopPropagation()}
        aria-label={ariaLabel}
        className={inputClassName ?? className}
        style={style}
      />
    );
  }

  return (
    <span
      id={id}
      role="button"
      tabIndex={0}
      aria-label={ariaLabel}
      onDoubleClick={(e) => {
        e.stopPropagation();
        enterEdit();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          enterEdit();
        }
      }}
      className={className}
      style={style}
    >
      {value}
    </span>
  );
}
