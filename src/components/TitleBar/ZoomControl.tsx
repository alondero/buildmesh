import { useEffect, useState } from 'react';
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
import { HeaderPillButton } from './HeaderPillButton';

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

interface IconProps {
  className?: string;
}

const ZOOM_DROPDOWN_ID = dropdownId('titlebar', 'zoom');

/** Lucide `zoom-in`. */
function ZoomIcon({ className }: IconProps) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <circle cx="11" cy="11" r="8" />
      <line x1="21" y1="21" x2="16.65" y2="16.65" />
      <line x1="11" y1="8" x2="11" y2="14" />
      <line x1="8" y1="11" x2="14" y2="11" />
    </svg>
  );
}

export function ZoomControl() {
  const [open, setOpen] = useState(false);
  const [size, setSize] = useState(terminalFontSize);

  // Reflect zoom changes from any source. The subscription is installed once
  // for the component's lifetime and released on unmount.
  useEffect(() => onTerminalFontSizeChange(setSize), []);

  useClickOutside(open ? ZOOM_DROPDOWN_ID : null, () => setOpen(false));
  useEscapeKey(() => setOpen(false), open);

  return (
    <div className="relative" data-dropdown-for={open ? ZOOM_DROPDOWN_ID : undefined}>
      <HeaderPillButton
        testId="titlebar-zoom"
        ariaLabel="Zoom terminal text size"
        ariaExpanded={open}
        ariaHasPopup="dialog"
        onClick={() => setOpen((value) => !value)}
        title="Terminal text size (Ctrl+= / Ctrl+- to zoom)"
        label="Zoom"
        active={open}
        icon={<ZoomIcon className="h-4 w-4 shrink-0" />}
      />
      {open && (
        <div
          role="dialog"
          aria-label="Terminal text size"
          data-testid="zoom-panel"
          className="absolute right-0 top-full z-50 mt-1 w-56 rounded-md border border-border-default bg-bg-card p-3 shadow-md animate-scale-in origin-top-right"
        >
          <div className="mb-2 flex items-center justify-between">
            <span className="text-xs text-text-secondary">Text size</span>
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
            data-testid="zoom-slider"
            className="w-full cursor-pointer accent-accent-cyan"
          />
          <div className="mt-2 flex items-center justify-between">
            <span className="text-[10px] leading-none text-text-muted">A</span>
            <button
              type="button"
              onClick={() => setTerminalFontSize(TERMINAL_FONT_SIZE_DEFAULT)}
              data-testid="zoom-reset"
              className="rounded-md px-1 text-[11px] text-text-muted transition-colors hover:text-accent-cyan"
            >
              Reset
            </button>
            <span className="text-base leading-none text-text-muted">A</span>
          </div>
        </div>
      )}
    </div>
  );
}
