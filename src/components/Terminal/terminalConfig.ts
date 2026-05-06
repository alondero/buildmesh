import type { ITerminalOptions } from '@xterm/xterm';

export const TERMINAL_OPTIONS: ITerminalOptions = {
  theme: {
    background: '#09090f',
    foreground: '#e2e8f0',
    cursor: '#00d4ff',
    selectionBackground: 'rgba(0, 212, 255, 0.15)',
  },
  fontSize: 10,
  fontFamily: 'JetBrains Mono, Fira Code, Cascadia Code, Consolas, monospace',
  fontWeight: 500,
  scrollback: 10000,
  cursorBlink: true,
  allowProposedApi: true,
};
