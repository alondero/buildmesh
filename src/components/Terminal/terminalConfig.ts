import type { ITerminalOptions } from '@xterm/xterm';
import { currentTheme, onThemeChange, type ThemeName } from '../../lib/theme';

export const TERMINAL_FONT_SIZE_MIN = 8;
export const TERMINAL_FONT_SIZE_MAX = 18;
export const TERMINAL_FONT_SIZE_DEFAULT = 10;

const STORAGE_KEY = 'terminal-font-size';

function loadFontSize(): number {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored !== null) {
      const parsed = parseInt(stored, 10);
      if (!isNaN(parsed) && parsed >= TERMINAL_FONT_SIZE_MIN && parsed <= TERMINAL_FONT_SIZE_MAX) {
        return parsed;
      }
    }
  } catch {
    // localStorage unavailable (e.g. test env)
  }
  return TERMINAL_FONT_SIZE_DEFAULT;
}

let _terminalFontSize = loadFontSize();

type FontSizeListener = (size: number) => void;
const listeners = new Set<FontSizeListener>();

export function terminalFontSize(): number {
  return _terminalFontSize;
}

export function setTerminalFontSize(size: number): void {
  const clamped = Math.max(TERMINAL_FONT_SIZE_MIN, Math.min(TERMINAL_FONT_SIZE_MAX, size));
  if (clamped === _terminalFontSize) return;
  _terminalFontSize = clamped;
  try {
    localStorage.setItem(STORAGE_KEY, String(clamped));
  } catch {
    // localStorage unavailable
  }
  listeners.forEach(cb => cb(clamped));
}

export function onTerminalFontSizeChange(cb: FontSizeListener): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

// ----- Theme palettes --------------------------------------------------------
//
// xterm.js takes literal hex/rgba strings — it does not read CSS variables —
// so each theme needs a hand-curated palette. These mirror the --color-*
// values in src/App.css ([data-theme="light"] override block). A future
// "let's soften the muted" PR must touch both the CSS override AND this
// file; the theme-tokens-light-contrast.test.ts test pins the values on
// both sides so the divergence is caught at the test boundary.
//
// Re-importing from theme.ts gives us the active theme for free at terminal
// construction time; TerminalRegistry's ThemeManager subscribes to
// onThemeChange and pushes the new palette to every live terminal on flip.

const DARK_TERMINAL_THEME = {
  background: '#0a0a0e',
  foreground: '#e2e8f0',
  cursor: '#00d4ff',
  selectionBackground: 'rgba(0, 212, 255, 0.15)',
} as const;

const LIGHT_TERMINAL_THEME = {
  // Mirrors --color-bg-base / --color-text-primary / --color-accent-cyan /
  // --color-accent-cyan-dim from the [data-theme="light"] block.
  background: '#fafafa',
  foreground: '#0f172a',
  cursor: '#0891b2',
  selectionBackground: 'rgba(8, 145, 178, 0.15)',
} as const;

export function terminalThemeFor(theme: ThemeName): ITerminalOptions['theme'] {
  return theme === 'light' ? LIGHT_TERMINAL_THEME : DARK_TERMINAL_THEME;
}

/**
 * Module-level theme-change subscriber used by TerminalRegistry's
 * ThemeManager. Kept separate from onTerminalFontSizeChange so a
 * future test fixture that wants to listen to one in isolation doesn't
 * have to unregister the other.
 */
export function onTerminalThemeChange(cb: (theme: ThemeName) => void): () => void {
  return onThemeChange(cb);
}

/**
 * Reads the current theme synchronously — used at terminal construction
 * time so a freshly opened terminal picks up whichever theme is active.
 * The pub/sub path (onTerminalThemeChange) handles live flips for already-
 * constructed terminals.
 */
export function activeTerminalTheme(): ITerminalOptions['theme'] {
  return terminalThemeFor(currentTheme());
}

const BASE_TERMINAL_OPTIONS: Omit<ITerminalOptions, 'fontSize' | 'theme'> = {
  fontFamily: 'JetBrains Mono, Fira Code, Cascadia Code, Consolas, monospace',
  fontWeight: 500,
  scrollback: 10000,
  cursorBlink: true,
  allowProposedApi: true,
};

export function createTerminalOptions(): ITerminalOptions {
  return {
    ...BASE_TERMINAL_OPTIONS,
    fontSize: terminalFontSize(),
    theme: activeTerminalTheme(),
  };
}

// Back-compat: TERMINAL_OPTIONS is still exported because the build-run
// test suite and a few other callers import it directly. Theme is pinned
// to the dark default (the historical behaviour) — any caller that needs
// the live theme should use createTerminalOptions() instead.
export const TERMINAL_OPTIONS: ITerminalOptions = {
  ...BASE_TERMINAL_OPTIONS,
  fontSize: TERMINAL_FONT_SIZE_DEFAULT,
  theme: DARK_TERMINAL_THEME,
};

export const SEARCH_DECORATIONS = {
  // Dark hex pair — search decorations sit on the terminal background, not
  // the page background, so they follow the terminal's palette. A future
  // enhancement could split this per-theme; for now the dark-on-dark
  // pattern still reads on the light terminal background (just lower
  // contrast — issue is filed separately, not part of #734).
  matchBackground: '#44403c',
  matchOverviewRuler: '#00d4ff',
  activeMatchBackground: '#00d4ff',
  activeMatchColorOverviewRuler: '#00d4ff',
} as const;
