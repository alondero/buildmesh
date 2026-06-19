import { describe, it, expect, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

// Mock frontendLog so the wrapper's log call is observable without going
// through the (already-tested) bridge. The wrapper imports the real module
// in production; this mock replaces it for the test.
vi.mock('../../src/lib/frontendLog', () => ({
  logFrontend: vi.fn(),
  installFrontendLogBridge: vi.fn(),
}));

// Import AFTER the mock so the wrapper picks up the mocked logFrontend.
import * as api from '../../src/lib/tauri';
import { logFrontend } from '../../src/lib/frontendLog';
import { shapeArgs } from '../../src/lib/ipcShape';

const mockInvoke = vi.mocked(invoke);
const mockLog = vi.mocked(logFrontend);

describe('central IPC error logging (#386)', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockLog.mockReset();
  });

  describe('shapeArgs', () => {
    it('numbers/booleans render as their type', () => {
      expect(shapeArgs({ id: 1, flag: true })).toEqual({ id: 'number', flag: 'boolean' });
    });

    it('strings render as N chars (PII-safe, size-safe)', () => {
      expect(shapeArgs({ data: 'ls -la' })).toEqual({ data: '6 chars' });
      expect(shapeArgs({ key: '' })).toEqual({ key: 'empty' });
    });

    it('arrays render as N items', () => {
      expect(shapeArgs({ ids: [1, 2, 3] })).toEqual({ ids: '3 items' });
      expect(shapeArgs({ ids: [] })).toEqual({ ids: 'empty' });
    });

    it('null and undefined render as their literal', () => {
      expect(shapeArgs({ a: null, b: undefined })).toEqual({ a: 'null', b: 'undefined' });
    });

    it('nested objects render as keys-only (no recursion into values)', () => {
      const shape = shapeArgs({ session: { id: 1, name: 'secret' } });
      expect(shape).toEqual({ session: '{ id, name }' });
    });

    it('binary buffers / typed arrays render as N bytes', () => {
      const buf = new Uint8Array(1024);
      expect(shapeArgs({ snapshot: buf })).toEqual({ snapshot: '1024 bytes' });
    });

    it('handles undefined / empty args', () => {
      expect(shapeArgs(undefined)).toEqual({});
      expect(shapeArgs(null)).toEqual({});
      expect(shapeArgs({})).toEqual({});
    });

    it('non-object args surface under a synthetic _value key', () => {
      expect(shapeArgs([1, 2, 3])).toEqual({ _value: '3 items' });
      expect(shapeArgs('hello')).toEqual({ _value: '5 chars' });
    });
  });

  describe('invoke wrapper', () => {
    it('does not call logFrontend on success', async () => {
      mockInvoke.mockResolvedValueOnce({ id: 1 });
      await api.getAgentNode(1);
      expect(mockLog).not.toHaveBeenCalled();
    });

    it('returns the resolved value unchanged', async () => {
      mockInvoke.mockResolvedValueOnce({ id: 7, name: 'x' });
      const result = await api.getAgentNode(7);
      expect(result).toEqual({ id: 7, name: 'x' });
    });

    it('logs command name + shape + error on rejection and re-throws', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('command not found'));
      await expect(api.spawnAgent(1, 'anthropic')).rejects.toThrow('command not found');
      expect(mockLog).toHaveBeenCalledTimes(1);
      const [level, msg] = mockLog.mock.calls[0];
      expect(level).toBe('error');
      expect(msg).toContain('[IPC:spawn_agent]');
      // Shape only — actual values never logged.
      expect(msg).not.toContain('anthropic');
      // The error text IS included (truncated), so a triage reader can
      // see "command not found" / "pty closed" / etc. without needing to
      // re-derive the failure from a separate log line.
      expect(msg).toContain('command not found');
    });

    it('preserves the original error for caller catches', async () => {
      const original = new Error('agent not running');
      mockInvoke.mockRejectedValueOnce(original);
      try {
        await api.writeToAgent(7, 'x');
        throw new Error('should have thrown');
      } catch (e) {
        expect(e).toBe(original);
      }
    });

    it('string-arg shapes show char count, not the value', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('boom'));
      await expect(api.writeToAgent(7, 'long-payload-here')).rejects.toThrow('boom');
      const msg = mockLog.mock.calls[0][1];
      expect(msg).toContain('write_to_agent');
      // `write_to_agent` is an `*_agent` IPC, which keeps the legacy
      // `sessionId` arg key (the agent-process level uses process-id
      // vocabulary per CONTEXT.md ambiguity #1). The number is printed
      // as the TYPE ('number'), not the value.
      expect(msg).toContain('"sessionId":"number"');
      expect(msg).toContain('17 chars');
      expect(msg).not.toContain('long-payload-here');
    });

    it('array-arg shapes show item count', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('boom'));
      await expect(api.deleteBranches('/p', ['a', 'b', 'c'])).rejects.toThrow('boom');
      const msg = mockLog.mock.calls[0][1];
      expect(msg).toContain('3 items');
    });

    it('handles commands with no args', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('boom'));
      await expect(api.listProviders()).rejects.toThrow('boom');
      const msg = mockLog.mock.calls[0][1];
      expect(msg).toContain('[IPC:list_providers]');
      expect(msg).toContain('{}');
    });

    it('caps the error text length to keep the log line bounded', async () => {
      const huge = 'x'.repeat(5000);
      mockInvoke.mockRejectedValueOnce(new Error(huge));
      await expect(api.listProviders()).rejects.toThrow();
      // The error text IS included in the message (truncated), so a
      // 5KB rejection doesn't blow up the log line. Wrapper cap is
      // modest — leave room for the prefix + shape.
      const msg = mockLog.mock.calls[0][1];
      expect(msg.length).toBeLessThan(500);
      expect(msg).toContain('…'); // truncation marker present
      // The full 5000-char error must not be in the message
      expect(msg).not.toContain('x'.repeat(5000));
    });

    it('hot-path commands still log (write_to_agent etc.)', async () => {
      // The user explicitly opted in to logging failures even on hot paths:
      // a write_to_agent failure means the agent died, which is exactly the
      // case we want visibility on.
      mockInvoke.mockRejectedValueOnce(new Error('pty closed'));
      await expect(api.writeToAgent(3, 'q')).rejects.toThrow('pty closed');
      expect(mockLog).toHaveBeenCalledTimes(1);
      expect(mockLog.mock.calls[0][1]).toContain('write_to_agent');
    });
  });
});
