import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { FontSizeManager } from '../../src/components/Terminal/FontSizeManager';
import {
  TERMINAL_FONT_SIZE_DEFAULT,
  setTerminalFontSize,
} from '../../src/components/Terminal/terminalConfig';

describe('FontSizeManager', () => {
  let manager: FontSizeManager;

  beforeEach(() => {
    manager = new FontSizeManager();
  });

  afterEach(() => {
    manager.destroy();
    // `_terminalFontSize` is a module-level singleton, so a test that zooms
    // would otherwise leak that value into every later test in this file
    // (and make the next `setTerminalFontSize` a no-op if it happened to
    // pick the same number). Reset to the default for isolation.
    setTerminalFontSize(TERMINAL_FONT_SIZE_DEFAULT);
  });

  describe('register/unregister', () => {
    it('tracks registered entries', () => {
      const terminal = { options: { fontSize: 10 } };
      manager.register(1, terminal, vi.fn());
      expect(manager.has(1)).toBe(true);
      expect(manager.size).toBe(1);
    });

    it('unregister removes entry', () => {
      const terminal = { options: { fontSize: 10 } };
      manager.register(1, terminal, vi.fn());
      manager.unregister(1);
      expect(manager.has(1)).toBe(false);
      expect(manager.size).toBe(0);
    });

    it('supports multiple entries', () => {
      manager.register(1, { options: { fontSize: 10 } }, vi.fn());
      manager.register(2, { options: { fontSize: 10 } }, vi.fn());
      manager.register(3, { options: { fontSize: 10 } }, vi.fn());
      expect(manager.size).toBe(3);
    });
  });

  describe('font size propagation', () => {
    it('updates terminal fontSize when global size changes', () => {
      const terminal = { options: { fontSize: 10 } };
      manager.register(1, terminal, vi.fn());

      setTerminalFontSize(14);
      expect(terminal.options.fontSize).toBe(14);
    });

    it('calls measureAndFit when font size changes', () => {
      const measureAndFit = vi.fn();
      manager.register(1, { options: { fontSize: 10 } }, measureAndFit);

      setTerminalFontSize(12);
      expect(measureAndFit).toHaveBeenCalledOnce();
    });

    it('propagates to all registered entries', () => {
      const fit1 = vi.fn();
      const fit2 = vi.fn();
      const term1 = { options: { fontSize: 10 } };
      const term2 = { options: { fontSize: 10 } };

      manager.register(1, term1, fit1);
      manager.register(2, term2, fit2);

      setTerminalFontSize(16);

      expect(term1.options.fontSize).toBe(16);
      expect(term2.options.fontSize).toBe(16);
      expect(fit1).toHaveBeenCalledOnce();
      expect(fit2).toHaveBeenCalledOnce();
    });

    it('does not call measureAndFit for unregistered entries', () => {
      const fit = vi.fn();
      manager.register(1, { options: { fontSize: 10 } }, fit);
      manager.unregister(1);

      setTerminalFontSize(14);
      expect(fit).not.toHaveBeenCalled();
    });
  });

  describe('destroy', () => {
    it('stops listening after destroy', () => {
      const terminal = { options: { fontSize: 10 } };
      const fit = vi.fn();
      manager.register(1, terminal, fit);

      manager.destroy();

      setTerminalFontSize(18);
      expect(fit).not.toHaveBeenCalled();
      expect(terminal.options.fontSize).toBe(10);
    });

    it('clears all entries on destroy', () => {
      manager.register(1, { options: { fontSize: 10 } }, vi.fn());
      manager.register(2, { options: { fontSize: 10 } }, vi.fn());

      manager.destroy();
      expect(manager.size).toBe(0);
    });
  });

  // The build/run registry keys terminals by a composite
  // `sessionId|mode|useWorktree` string rather than a numeric node id, so the
  // manager has to accept string keys too.
  describe('string keys (build/run composite keys)', () => {
    it('propagates the global size to a string-keyed entry and unregisters it', () => {
      const stringManager = new FontSizeManager();
      const terminal = { options: { fontSize: 10 } };
      const fit = vi.fn();
      stringManager.register('9|build|false', terminal, fit);
      expect(stringManager.has('9|build|false')).toBe(true);

      setTerminalFontSize(14);

      expect(terminal.options.fontSize).toBe(14);
      expect(fit).toHaveBeenCalledOnce();

      stringManager.unregister('9|build|false');
      expect(stringManager.size).toBe(0);
      stringManager.destroy();
    });
  });

  describe('fan-out isolation', () => {
    it('keeps notifying other entries after one measureAndFit throws', () => {
      const brokenFit = vi.fn(() => {
        throw new Error('pane mid-teardown');
      });
      const healthyFit = vi.fn();
      const healthyTerminal = { options: { fontSize: 10 } };
      const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});

      manager.register(1, { options: { fontSize: 10 } }, brokenFit);
      manager.register(2, healthyTerminal, healthyFit);

      expect(() => setTerminalFontSize(16)).not.toThrow();

      // The healthy pane still got the new size + a refit despite the
      // sibling throwing first.
      expect(healthyTerminal.options.fontSize).toBe(16);
      expect(healthyFit).toHaveBeenCalledOnce();
      warn.mockRestore();
    });
  });
});
