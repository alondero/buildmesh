import { onTerminalFontSizeChange } from './terminalConfig';

type FitFn = () => void;

interface TerminalLike {
  options: { fontSize?: number };
}

interface ManagedEntry {
  terminal: TerminalLike;
  measureAndFit: FitFn;
}

/** A registry-key for a Terminal instance — the agent terminal uses
 *  `nodeId` (number); the build/run terminal uses the composite
 *  `(sessionId, mode, useWorktree)` tuple serialised to a string. Mirrors
 *  `ThemeKey` in ThemeManager so the two sibling managers stay aligned. */
export type FontSizeKey = number | string;

export class FontSizeManager {
  private entries = new Map<FontSizeKey, ManagedEntry>();
  private unlisten: () => void;

  constructor() {
    this.unlisten = onTerminalFontSizeChange((size) => {
      for (const entry of this.entries.values()) {
        entry.terminal.options.fontSize = size;
        // A pane mid-teardown or not yet laid out can throw from inside
        // xterm's measurement. Isolate each entry so one bad pane can't
        // strand every other registered terminal at the old size.
        try {
          entry.measureAndFit();
        } catch (error) {
          console.warn('[FontSizeManager] measureAndFit failed during zoom fan-out:', error);
        }
      }
    });
  }

  register(key: FontSizeKey, terminal: TerminalLike, measureAndFit: FitFn): void {
    this.entries.set(key, { terminal, measureAndFit });
  }

  unregister(key: FontSizeKey): void {
    this.entries.delete(key);
  }

  has(key: FontSizeKey): boolean {
    return this.entries.has(key);
  }

  get size(): number {
    return this.entries.size;
  }

  destroy(): void {
    this.unlisten();
    this.entries.clear();
  }
}
