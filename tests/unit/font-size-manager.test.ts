import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { FontSizeManager } from '../../src/components/Terminal/FontSizeManager';
import { setTerminalFontSize } from '../../src/components/Terminal/terminalConfig';

describe('FontSizeManager', () => {
  let manager: FontSizeManager;

  beforeEach(() => {
    manager = new FontSizeManager();
  });

  afterEach(() => {
    manager.destroy();
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
});
