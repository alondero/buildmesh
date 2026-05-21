import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  installFrontendLogBridge,
  logFrontend,
  _resetFrontendLogBridgeForTests,
} from '../../src/lib/frontendLog';

describe('frontendLog bridge', () => {
  let origError: typeof console.error;
  let origWarn: typeof console.warn;

  beforeEach(() => {
    _resetFrontendLogBridgeForTests();
    origError = console.error;
    origWarn = console.warn;
    vi.mocked(invoke).mockClear();
    vi.mocked(invoke).mockResolvedValue(undefined);
  });

  afterEach(() => {
    console.error = origError;
    console.warn = origWarn;
  });

  it('logFrontend forwards level + message to log_frontend command', () => {
    logFrontend('error', 'boom');
    expect(invoke).toHaveBeenCalledWith('log_frontend', { level: 'error', message: 'boom' });
  });

  it('install patches console.error to forward and still call original', () => {
    const spy = vi.fn();
    console.error = spy;
    installFrontendLogBridge();

    console.error('something broke', { detail: 42 });

    expect(spy).toHaveBeenCalledWith('something broke', { detail: 42 });
    expect(invoke).toHaveBeenCalledWith('log_frontend', {
      level: 'error',
      message: 'something broke {"detail":42}',
    });
  });

  it('install patches console.warn to forward', () => {
    const spy = vi.fn();
    console.warn = spy;
    installFrontendLogBridge();

    console.warn('careful');

    expect(spy).toHaveBeenCalledWith('careful');
    expect(invoke).toHaveBeenCalledWith('log_frontend', {
      level: 'warn',
      message: 'careful',
    });
  });

  it('serializes Error instances with stack', () => {
    installFrontendLogBridge();
    const err = new Error('kaboom');
    err.stack = 'Error: kaboom\n    at fn (file.ts:10)';

    console.error(err);

    const call = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    expect(call).toBeDefined();
    const message = (call![1] as { message: string }).message;
    expect(message).toContain('Error: kaboom');
    expect(message).toContain('at fn (file.ts:10)');
  });

  it('does not throw if invoke rejects', async () => {
    installFrontendLogBridge();
    vi.mocked(invoke).mockRejectedValueOnce(new Error('command not found'));

    expect(() => console.error('safe?')).not.toThrow();
    // Let the rejected promise settle without an unhandledrejection.
    await new Promise(r => setTimeout(r, 0));
  });

  it('install is idempotent', () => {
    const spy = vi.fn();
    console.error = spy;
    installFrontendLogBridge();
    installFrontendLogBridge();

    console.error('once');

    // Spy called once (one underlying patch), invoke called once (no double-forward).
    expect(spy).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it('forwards window.error events', () => {
    installFrontendLogBridge();
    const event = new ErrorEvent('error', {
      message: 'sync throw',
      filename: 'app.js',
      lineno: 42,
      colno: 7,
      error: new Error('sync throw'),
    });
    window.dispatchEvent(event);

    const call = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    expect(call).toBeDefined();
    const payload = call![1] as { level: string; message: string };
    expect(payload.level).toBe('error');
    expect(payload.message).toContain('window.error');
    expect(payload.message).toContain('app.js:42:7');
  });

  it('forwards unhandledrejection events', () => {
    installFrontendLogBridge();
    const event = new Event('unhandledrejection') as PromiseRejectionEvent;
    Object.defineProperty(event, 'reason', { value: new Error('async fail') });
    window.dispatchEvent(event);

    const call = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    expect(call).toBeDefined();
    const payload = call![1] as { level: string; message: string };
    expect(payload.level).toBe('error');
    expect(payload.message).toContain('unhandledrejection');
    expect(payload.message).toContain('async fail');
  });
});
