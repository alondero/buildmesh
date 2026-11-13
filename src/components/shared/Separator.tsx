/**
 * Separator — the shared accessible pane-separator primitive (issue #2424).
 *
 * The sidebar, the Probe panel, the agent grid's column and row
 * splitters, and the two-pane `ResizablePanes` row are drag handles that
 * were reachable by pointer only. This component is the ONE place that
 * turns such a handle into an operable control, so the call sites do not
 * drift apart on the parts assistive technology depends on:
 *
 *   - `role="separator"` with `aria-orientation`, so a screen reader
 *     announces WHAT the control is and along WHICH axis it moves.
 *   - `aria-label` — the accessible name ("Resize sidebar", "Resize row 1
 *     of 2", …). Without it the separator is announced without a name,
 *     and the panes it separates are indistinguishable to the user.
 *   - `aria-valuenow` / `aria-valuemin` / `aria-valuemax` — the current
 *     size and the range it may move within, so the user hears the
 *     limits before they hit them.
 *   - `tabIndex={0}` — the handle sits in the tab order.
 *   - a visible focus indicator (`focus-visible` outline in the app's
 *     accent token) — a keyboard-only user must be able to see where
 *     they are.
 *   - the WAI-ARIA separator key map: arrow keys step the size, Shift
 *     multiplies the step, PageUp/PageDown take a coarse step, and
 *     Home/End jump to the ends of the range.
 *
 * Key map (the "value" is the size of the pane the separator controls):
 *
 *   | Key       | vertical separator        | horizontal separator      |
 *   |-----------|---------------------------|---------------------------|
 *   | grow      | ArrowRight                | ArrowDown                 |
 *   | shrink    | ArrowLeft                 | ArrowUp                   |
 *   | coarse    | PageUp (grow)             | PageDown (grow)           |
 *   | Home      | maximum                   | maximum                   |
 *   | End       | minimum                   | minimum                   |
 *
 * The arrow that GROWS the controlled pane is the one that moves the
 * separator towards the far edge of that pane: for a vertical separator
 * the pane in front of the separator is on the LEFT, so ArrowRight
 * grows it; for a horizontal separator the pane in front is on TOP, so
 * ArrowDown grows it. `reverse` flips that mapping for the one surface
 * whose controlled pane sits AFTER the separator — the Probe panel's
 * handle is on the panel's LEFT edge, so ArrowLeft grows the panel.
 *
 * Home/End jump to the maximum/minimum (not the APG's minimum/maximum):
 * that is the convention the Probe panel already shipped with, and the
 * app's own screenshot scripts drive it (`End` → narrowest Probe,
 * `Home` → widest Probe). Changing it would break them for no gain.
 *
 * The component is deliberately UNCONTROLLED in shape but fully
 * controlled in practice: the caller owns the value, clamps it, and
 * persists it. This component only computes the next value and hands it
 * over — it owns no store, no persistence and no layout.
 */
import {
  useRef,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
} from 'react';

export type SeparatorOrientation = 'vertical' | 'horizontal';

export interface SeparatorProps {
  /**
   * The accessible name. Screen readers announce it instead of an
   * anonymous "separator", so it must say WHICH pane the handle moves
   * ("Resize sidebar", "Resize row 1 of 2").
   */
  label: string;
  /**
   * The axis the separator moves along. `vertical` = the handle moves
   * left/right (a column splitter); `horizontal` = up/down (a row
   * splitter).
   */
  orientation?: SeparatorOrientation;
  /**
   * The current size of the controlled pane, in the same unit as
   * `min`/`max` (pixels — the unit the call sites use today).
   */
  value: number;
  /** The smallest value the pane may take. */
  min: number;
  /** The largest value the pane may take. */
  max: number;
  /**
   * Called with the next value when the user resizes with the keyboard.
   * The value is already clamped to [min, max]; the caller still owns
   * the persistence and the terminal refit.
   */
  onResize: (next: number) => void;
  /**
   * True when the controlled pane sits AFTER the separator (the Probe
   * panel's handle is on its left edge). Flips the arrow-key direction.
   */
  reverse?: boolean;
  /** Pointer-drag entry point — the `useResizable` mousedown handler. */
  onMouseDown?: (event: ReactMouseEvent<HTMLDivElement>) => void;
  /**
   * A live reading of the current value, read at keydown time. Most
   * callers do not need it: the `value` prop is already the latest
   * committed size. The Probe panel does — its width is a DOM
   * measurement, and a resize observer reports it a frame later than a
   * keydown can arrive, so a held arrow key would otherwise repeat a
   * step instead of taking the next one.
   */
  getValue?: () => number;
  /** Small keyboard step, in value units. Defaults to 8 (pixels). */
  step?: number;
  /** Coarse keyboard step, in value units. Defaults to 80 (pixels). */
  pageStep?: number;
  /** Extra classes for the handle's own layout (position, cursor, …). */
  className?: string;
  /** Inline styles for the handle's own box (size, flex, …). */
  style?: CSSProperties;
  'data-testid'?: string;
}

/**
 * The focus indicator is painted with the app's accent token, matching
 * the ring the rest of the app uses (DESIGN.md). `focus-visible:z-20`
 * lifts the focused handle above its neighbouring panes so the ring is
 * not painted over by the pane that follows it in the DOM — without it
 * a grid splitter (5px wide, between two opaque panes) would show half
 * a ring.
 *
 * `focus-visible:outline-solid` is load-bearing, not decoration.
 * Tailwind v4's `outline-<width>` utility emits
 * `outline-style: var(--tw-outline-style)`, and the base `outline-none`
 * sets that very property to `none` on this element — so `outline-2`
 * alone resolves to `outline-style: none` and the ring does not paint.
 * The `outline-solid` variant sets the property back to `solid` with the
 * focus variant's higher specificity. (The app's own
 * `[role='separator']:focus-visible` rule in `App.css` does not cover it:
 * it sits in the `base` layer, and Tailwind's `utilities` layer — where
 * `outline-none` lives — is applied after it.)
 */
const BASE_CLASS =
  'outline-none focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-accent-cyan focus-visible:z-20';

const clamp = (n: number, min: number, max: number) => Math.max(min, Math.min(max, n));

export function Separator({
  label,
  orientation = 'vertical',
  value,
  min,
  max,
  onResize,
  reverse = false,
  onMouseDown,
  getValue,
  step = 8,
  pageStep = 80,
  className,
  style,
  'data-testid': testId,
}: SeparatorProps) {
  // The value is mirrored into a ref so an auto-repeat keydown reads the
  // latest committed size rather than the one captured by the render
  // that installed the handler — the same discipline `useResizable`
  // applies to its drag baseline (issue #301).
  const valueRef = useRef(value);
  valueRef.current = value;

  const vertical = orientation === 'vertical';
  const growKey = vertical
    ? (reverse ? 'ArrowLeft' : 'ArrowRight')
    : (reverse ? 'ArrowUp' : 'ArrowDown');
  const shrinkKey = vertical
    ? (reverse ? 'ArrowRight' : 'ArrowLeft')
    : (reverse ? 'ArrowDown' : 'ArrowUp');
  // PageUp/PageDown are off-axis keys for a vertical separator, so they
  // follow the APG convention there (PageUp grows). For a horizontal
  // separator they ARE the axis, so they follow the arrow keys.
  const pageGrowKey = vertical ? 'PageUp' : 'PageDown';
  const pageShrinkKey = vertical ? 'PageDown' : 'PageUp';

  const handleKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    // A caller-supplied live reading wins: it is measured at keydown
    // time, so a held arrow key does not repeat a step.
    const measured = getValue?.();
    const current = typeof measured === 'number' && Number.isFinite(measured)
      ? measured
      : (Number.isFinite(valueRef.current) ? valueRef.current : min);
    // Shift is the coarse modifier: 4× the arrow step, and the coarse
    // step becomes a jump across the whole range.
    const small = event.shiftKey ? step * 4 : step;
    const large = event.shiftKey ? Math.max(0, max - min) : pageStep;

    let next: number | null = null;
    switch (event.key) {
      case growKey: next = current + small; break;
      case shrinkKey: next = current - small; break;
      case pageGrowKey: next = current + large; break;
      case pageShrinkKey: next = current - large; break;
      case 'Home': next = max; break;
      case 'End': next = min; break;
      default: return;
    }

    event.preventDefault();
    onResize(clamp(next, min, max));
  };

  const safeValue = Number.isFinite(value) ? value : min;

  return (
    <div
      role="separator"
      aria-orientation={orientation}
      aria-label={label}
      aria-valuenow={Math.round(safeValue)}
      aria-valuemin={Math.round(min)}
      aria-valuemax={Math.round(max)}
      tabIndex={0}
      onMouseDown={onMouseDown}
      onKeyDown={handleKeyDown}
      className={className ? `${BASE_CLASS} ${className}` : BASE_CLASS}
      style={style}
      data-testid={testId}
    />
  );
}
