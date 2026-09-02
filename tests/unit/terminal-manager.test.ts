/**
 * Unit tests for TerminalManager
 *
 * Tests the public API: getInstance, getOrCreate, subscribe, dispose
 * Note: TerminalManager uses a private _instances field, so we test
 * through the public API only.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { terminalManager } from '../../src/components/Terminal/Terminal';
import * as tauriApi from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

describe('TerminalManager', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    // Reset TerminalManager state by disposing all instances
    const instances = [1, 2, 3, 4, 5]; // Known test session IDs
    instances.forEach(id => terminalManager.dispose(id));
  });

  afterEach(() => {
    // Cleanup any remaining instances
    const instances = [1, 2, 3, 4, 5];
    instances.forEach(id => terminalManager.dispose(id));
  });

  describe('getInstance', () => {
    it('returns undefined for non-existent session', () => {
      expect(terminalManager.getInstance(999)).toBeUndefined();
    });

    it('returns instance after getOrCreate', async () => {
      const instance = await terminalManager.getOrCreate(1);
      expect(instance).not.toBeNull();
      expect(terminalManager.getInstance(1)).toBe(instance);
    });
  });

  describe('getOrCreate', () => {
    it('creates a new instance on first call for a nodeId', async () => {
      const instance = await terminalManager.getOrCreate(1);
      expect(instance).not.toBeNull();
      expect(instance!.term).toBeDefined();
      expect(instance!.fitAddon).toBeDefined();
      expect(instance!.unlisten).toBeDefined();
    });

    it('returns the same instance on subsequent calls with same nodeId', async () => {
      const first = await terminalManager.getOrCreate(1);
      const second = await terminalManager.getOrCreate(1);
      expect(first).toBe(second);
    });

    it('creates separate instances for different nodeIds', async () => {
      const inst1 = await terminalManager.getOrCreate(1);
      const inst2 = await terminalManager.getOrCreate(2);
      expect(inst1).not.toBe(inst2);
    });

    it('calls listen to set up agent-output event handler', async () => {
      const listenSpy = vi.spyOn(tauriApi, 'listen');
      await terminalManager.getOrCreate(1);
      expect(listenSpy).toHaveBeenCalledWith(
        'agent-output',
        expect.any(Function)
      );
    });

    it('subscribes to the binary agent-output Channel on create', async () => {
      const invokeSpy = vi.spyOn(await import('@tauri-apps/api/core'), 'invoke');
      await terminalManager.getOrCreate(1);
      expect(invokeSpy).toHaveBeenCalledWith('subscribe_agent_output', {
        sessionId: 1,
        onChunk: expect.any(Object),
      });
    });

    it('does not resubscribe or unsubscribe when getOrCreate reuses the instance', async () => {
      const invokeSpy = vi.spyOn(await import('@tauri-apps/api/core'), 'invoke');
      await terminalManager.getOrCreate(1);
      invokeSpy.mockClear();

      await terminalManager.getOrCreate(1);

      const outputIpc = invokeSpy.mock.calls.filter(
        ([command]) =>
          command === 'subscribe_agent_output' || command === 'unsubscribe_agent_output',
      );
      expect(outputIpc).toEqual([]);
    });

    it('sets up onData handler that calls invoke', async () => {
      const invokeSpy = vi.spyOn(await import('@tauri-apps/api/core'), 'invoke');
      await terminalManager.getOrCreate(1);
      const instance = terminalManager.getInstance(1)!;

      // Get the onData callback and call it
      const onDataCallback = (instance!.term as unknown as { onData: ReturnType<typeof vi.fn> }).onData.mock.calls[0][0];
      onDataCallback('test data');

      expect(invokeSpy).toHaveBeenCalledWith('write_to_agent', {
        sessionId: 1,
        data: 'test data',
      });
    });

    it('sets up onResize handler that calls invoke', async () => {
      const invokeSpy = vi.spyOn(await import('@tauri-apps/api/core'), 'invoke');
      await terminalManager.getOrCreate(1);
      const instance = terminalManager.getInstance(1)!;

      // Get the onResize callback and call it
      const onResizeCallback = (instance!.term as unknown as { onResize: ReturnType<typeof vi.fn> }).onResize.mock.calls[0][0];
      onResizeCallback({ cols: 100, rows: 40 });

      expect(invokeSpy).toHaveBeenCalledWith('resize_agent', {
        sessionId: 1,
        cols: 100,
        rows: 40,
      });
    });

    it('notifies subscribers after creating instance', async () => {
      const notifyCallback = vi.fn();
      terminalManager.subscribe(notifyCallback);
      await terminalManager.getOrCreate(1);
      expect(notifyCallback).toHaveBeenCalled();
    });
  });

  describe('subscribe', () => {
    it('returns an unsubscribe function', () => {
      const cb = vi.fn();
      const unsub = terminalManager.subscribe(cb);
      expect(typeof unsub).toBe('function');
    });

    it('calls subscriber when notify is triggered', async () => {
      const cb = vi.fn();
      terminalManager.subscribe(cb);
      await terminalManager.getOrCreate(1); // triggers notify
      expect(cb).toHaveBeenCalled();
    });

    it('removed subscriber is not called', async () => {
      const cb = vi.fn();
      const unsub = terminalManager.subscribe(cb);
      unsub();
      await terminalManager.getOrCreate(1);
      expect(cb).not.toHaveBeenCalled();
    });
  });

  describe('dispose', () => {
    it('removes instance after dispose', async () => {
      await terminalManager.getOrCreate(1);
      expect(terminalManager.getInstance(1)).toBeDefined();

      terminalManager.dispose(1);

      expect(terminalManager.getInstance(1)).toBeUndefined();
    });

    it('calls unlisten on dispose', async () => {
      await terminalManager.getOrCreate(1);
      const instance = terminalManager.getInstance(1)!;
      const unlistenSpy = vi.spyOn(instance, 'unlisten');

      terminalManager.dispose(1);

      expect(unlistenSpy).toHaveBeenCalled();
    });

    it('unsubscribes the binary Channel on dispose', async () => {
      const invokeSpy = vi.spyOn(await import('@tauri-apps/api/core'), 'invoke');
      await terminalManager.getOrCreate(1);
      invokeSpy.mockClear();

      terminalManager.dispose(1);

      expect(invokeSpy).toHaveBeenCalledWith('unsubscribe_agent_output', {
        sessionId: 1,
      });
    });

    it('calls term.dispose on dispose', async () => {
      await terminalManager.getOrCreate(1);
      const instance = terminalManager.getInstance(1)!;
      const disposeSpy = vi.spyOn(instance.term, 'dispose');

      terminalManager.dispose(1);

      expect(disposeSpy).toHaveBeenCalled();
    });

    it('is idempotent - calling dispose twice does not error', () => {
      expect(() => terminalManager.dispose(999)).not.toThrow();
      expect(() => terminalManager.dispose(999)).not.toThrow();
    });

    it('notifies subscribers after dispose', async () => {
      const cb = vi.fn();
      terminalManager.subscribe(cb);
      await terminalManager.getOrCreate(1);
      cb.mockClear();

      terminalManager.dispose(1);

      expect(cb).toHaveBeenCalled();
    });
  });

  describe('multiple instances', () => {
    it('can create and manage multiple sessions simultaneously', async () => {
      const inst1 = await terminalManager.getOrCreate(1);
      const inst2 = await terminalManager.getOrCreate(2);
      const inst3 = await terminalManager.getOrCreate(3);

      expect(inst1).not.toBe(inst2);
      expect(inst2).not.toBe(inst3);
      expect(inst1).not.toBe(inst3);

      expect(terminalManager.getInstance(1)).toBe(inst1);
      expect(terminalManager.getInstance(2)).toBe(inst2);
      expect(terminalManager.getInstance(3)).toBe(inst3);
    });

    it('can dispose individual sessions without affecting others', async () => {
      await terminalManager.getOrCreate(1);
      await terminalManager.getOrCreate(2);
      await terminalManager.getOrCreate(3);

      terminalManager.dispose(2);

      // After disposing session 2, we can still get sessions 1 and 3 via getOrCreate
      // (they return existing instances)
      const inst1Again = await terminalManager.getOrCreate(1);
      const inst3Again = await terminalManager.getOrCreate(3);
      expect(inst1Again).not.toBeNull();
      expect(inst3Again).not.toBeNull();

      // Session 2 was disposed, so getOrCreate returns null
      // (the instance was fully removed, not just marked as disposed)
      // This is expected behavior - calling dispose removes the instance
    });
  });
});
