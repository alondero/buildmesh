import { onTerminalFontSizeChange } from './terminalConfig';

type FitFn = () => void;

interface TerminalLike {
  options: { fontSize?: number };
}

interface ManagedEntry {
  terminal: TerminalLike;
  measureAndFit: FitFn;
}

export class FontSizeManager {
  private entries = new Map<number, ManagedEntry>();
  private unlisten: () => void;

  constructor() {
    this.unlisten = onTerminalFontSizeChange((size) => {
      for (const entry of this.entries.values()) {
        entry.terminal.options.fontSize = size;
        entry.measureAndFit();
      }
    });
  }

  register(nodeId: number, terminal: TerminalLike, measureAndFit: FitFn): void {
    this.entries.set(nodeId, { terminal, measureAndFit });
  }

  unregister(nodeId: number): void {
    this.entries.delete(nodeId);
  }

  has(nodeId: number): boolean {
    return this.entries.has(nodeId);
  }

  get size(): number {
    return this.entries.size;
  }

  destroy(): void {
    this.unlisten();
    this.entries.clear();
  }
}
