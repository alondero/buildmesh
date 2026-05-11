import type { ITerminalOptions } from '@xterm/xterm';

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

const BASE_TERMINAL_OPTIONS: Omit<ITerminalOptions, 'fontSize'> = {
  theme: {
    background: '#09090f',
    foreground: '#e2e8f0',
    cursor: '#00d4ff',
    selectionBackground: 'rgba(0, 212, 255, 0.15)',
  },
  fontFamily: 'JetBrains Mono, Fira Code, Cascadia Code, Consolas, monospace',
  fontWeight: 500,
  scrollback: 10000,
  cursorBlink: true,
  allowProposedApi: true,
};

export function createTerminalOptions(): ITerminalOptions {
  return { ...BASE_TERMINAL_OPTIONS, fontSize: terminalFontSize() };
}

export const TERMINAL_OPTIONS: ITerminalOptions = BASE_TERMINAL_OPTIONS as ITerminalOptions;
