import { onTerminalThemeChange, terminalThemeFor } from './terminalConfig';
import type { ThemeName } from '../../lib/theme';

interface TerminalLike {
  options: { theme?: object };
}

/** A registry-key for a Terminal instance — the agent terminal uses
 *  `nodeId` (number); the build/run terminal uses the composite
 *  `(sessionId, mode, useWorktree)` tuple serialised to a string. Both
 *  maps support string|number keys so a single ThemeManager can serve
 *  either registry (issue #734). */
export type ThemeKey = number | string;

/**
 * Pushes the active xterm.js theme to every registered Terminal instance
 * when the app theme flips (issue #734). Mirrors the FontSizeManager
 * pattern: subscribes to a module-level pub/sub at construction, walks
 * registered entries on each flip, releases its listener on destroy().
 *
 * Kept separate from FontSizeManager so the two concerns can be tested
 * in isolation (each owns its own listener + entry map). Both the
 * agent-terminal and build-run registries compose their own
 * ThemeManager — sharing one would couple the two lifecycles (a
 * disposed build-run terminal would have to know about a separate
 * registry's destroy call). Two listeners is cheap; one shared map
 * would have been a footgun.
 */
export class ThemeManager {
  private entries = new Map<ThemeKey, TerminalLike>();
  private unlisten: () => void;

  constructor() {
    this.unlisten = onTerminalThemeChange((theme) => {
      const palette = terminalThemeFor(theme);
      for (const entry of this.entries.values()) {
        entry.options.theme = palette;
      }
    });
  }

  register(key: ThemeKey, terminal: TerminalLike): void {
    this.entries.set(key, terminal);
  }

  unregister(key: ThemeKey): void {
    this.entries.delete(key);
  }

  has(key: ThemeKey): boolean {
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

export type { ThemeName };