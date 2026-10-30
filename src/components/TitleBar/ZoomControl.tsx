import { useCallback, useEffect, useId, useRef, useState } from 'react';
import {
  TERMINAL_FONT_SIZE_DEFAULT,
  TERMINAL_FONT_SIZE_MAX,
  TERMINAL_FONT_SIZE_MIN,
  onTerminalFontSizeChange,
  setTerminalFontSize,
  terminalFontSize,
} from '../Terminal/terminalConfig';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useEscapeKey } from '../../hooks/useEscapeKey';
import { dropdownId } from '../../lib/dropdownId';
import { SHORTCUT_CATALOG, shortcutLabel } from '../../lib/shortcutCatalog';
import { HeaderPillButton } from './HeaderPillButton';
import { ZoomIcon } from './utilityIcons';

/**
 * Terminal text-size control for the title bar.
 *
 * There is deliberately no new state here: `terminalConfig` is already the
 * single source of truth for the terminal font size, and every input path
 * (Ctrl/Cmd `0`/`=`/`-`, Ctrl/Cmd+wheel, and now this slider) funnels
 * through `setTerminalFontSize`. The control writes through the same setter
 * and subscribes to `onTerminalFontSizeChange`, so the slider and its
 * readout track keyboard/wheel changes as they happen — no polling, no
 * duplicated store.
 */

const ZOOM_DROPDOWN_ID = dropdownId('titlebar', 'zoom');

/** Resolve the zoom chord labels once at module scope (mirrors TitleBar's
 *  `OMNIBAR_CATALOG_ENTRY`) so the trigger's tooltip tracks the cheatsheet on
 *  both platforms — ⌘ on macOS, Ctrl elsewhere — and can't drift from the
 *  actual binding. */
function zoomHint(): string {
  const labelFor = (action: string) => {
    const entry = SHORTCUT_CATALOG.find((candidate) => candidate.action === action);
    return entry ? shortcutLabel(entry) : '';
  };
  const inLabel = labelFor('zoom-in');
  const outLabel = labelFor('zoom-out');
  return inLabel !== '' && outLabel !== '' ? `${inLabel} / ${outLabel} to zoom` : 'zoom';
}

const ZOOM_TITLE = `Terminal text size (${zoomHint()})`;

/**
 * The text-size slider, readout and reset — the zoom popover's body, and the
 * "Terminal text size" row of the More panel when the Zoom pill folds into it
 * below `UTILITIES_IN_BAR_MIN_WIDTH` (#2421). Writes through
 * `setTerminalFontSize` and follows `onTerminalFontSizeChange`, so every
 * mounted copy tracks the keyboard and wheel shortcuts live.
 */
export function TerminalTextSize({ title = 'Text size' }: { title?: string }) {
  const [size, setSize] = useState(terminalFontSize);
  // Reflect zoom changes from any source for this copy's lifetime.
  useEffect(() => onTerminalFontSizeChange(setSize), []);
  return (
    <>
      <div className="mb-2 flex items-center justify-between">
        <span className="text-xs text-text-secondary">{title}</span>
        <span className="font-mono text-xs text-text-primary" data-testid="zoom-value">
          {size}px
        </span>
      </div>
      <input
        type="range"
        min={TERMINAL_FONT_SIZE_MIN}
        max={TERMINAL_FONT_SIZE_MAX}
        step={1}
        value={size}
        onChange={(e) => setTerminalFontSize(Number(e.target.value))}
        aria-label="Terminal text size"
        // Screen readers otherwise announce the bare slider number;
        // `aria-valuetext` supplies the unit the visible readout shows.
        aria-valuetext={`${size}px`}
        data-testid="zoom-slider"
        className="w-full cursor-pointer accent-accent-cyan"
      />
      <div className="mt-2 flex items-center justify-between">
        {/* Decorative size swatches — the slider carries the value for
            assistive tech, so these are hidden from the a11y tree. */}
        <span aria-hidden="true" className="text-2xs leading-none text-text-muted">A</span>
        <button
          type="button"
          onClick={() => setTerminalFontSize(TERMINAL_FONT_SIZE_DEFAULT)}
          disabled={size === TERMINAL_FONT_SIZE_DEFAULT}
          data-testid="zoom-reset"
          className="rounded-md px-1 text-sm text-text-muted transition-colors hover:text-accent-cyan disabled:cursor-default disabled:text-text-muted disabled:hover:text-text-muted"
        >
          Reset
        </button>
        <span aria-hidden="true" className="text-base leading-none text-text-muted">A</span>
      </div>
    </>
  );
}

export function ZoomControl() {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelId = useId();

  // Escape returns focus to the trigger — matching BuildRunDropdown's
  // disclosure contract — so a keyboard user isn't dumped at document.body.
  // Outside-click closes without stealing focus (the user clicked elsewhere),
  // hence the separate handler below.
  const closeAndReturnFocus = useCallback(() => {
    const trigger = triggerRef.current;
    setOpen(false);
    requestAnimationFrame(() => trigger?.focus());
  }, []);

  useClickOutside(open ? ZOOM_DROPDOWN_ID : null, () => setOpen(false));
  useEscapeKey(closeAndReturnFocus, open);

  return (
    <div className="relative" data-dropdown-for={open ? ZOOM_DROPDOWN_ID : undefined}>
      <HeaderPillButton
        buttonRef={triggerRef}
        testId="titlebar-zoom"
        ariaLabel="Zoom terminal text size"
        ariaExpanded={open}
        ariaHasPopup="dialog"
        ariaControls={open ? panelId : undefined}
        onClick={() => setOpen((value) => !value)}
        title={ZOOM_TITLE}
        label="Zoom"
        active={open}
        icon={<ZoomIcon className="h-4 w-4 shrink-0" />}
      />
      {open && (
        <div
          id={panelId}
          role="dialog"
          aria-label="Terminal text size"
          data-testid="zoom-panel"
          className="absolute right-0 top-full z-50 mt-1 w-56 rounded-md border border-border-default bg-bg-card p-3 shadow-md animate-scale-in origin-top-right"
        >
          <TerminalTextSize />
        </div>
      )}
    </div>
  );
}
