import { onTerminalFontSizeChange } from './terminalConfig';

type FitFn = () => void;

interface TerminalLike {
  options: { fontSize?: number };
}

interface ManagedEntry {
  terminal: TerminalLike;
  measureAndFit: FitFn;
}

/** Keyed by whatever identifies a terminal in its owning registry: `number`
 *  node ids for the agent registry, the composite `sessionId|mode|useWorktree`
 *  string for build/run panes. Defaults to `number` so existing call sites
 *  keep their signatures. */
export class FontSizeManager<K extends string | number = number> {
  private entries = new Map<K, ManagedEntry>();
  private unlisten: () => void;

  constructor() {
    this.unlisten = onTerminalFontSizeChange((size) => {
      for (const entry of this.entries.values()) {
        entry.terminal.options.fontSize = size;
        entry.measureAndFit();
      }
    });
  }

  register(key: K, terminal: TerminalLike, measureAndFit: FitFn): void {
    this.entries.set(key, { terminal, measureAndFit });
  }

  unregister(key: K): void {
    this.entries.delete(key);
  }

  has(key: K): boolean {
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
