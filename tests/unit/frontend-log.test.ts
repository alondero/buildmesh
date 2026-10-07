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
  let origInfo: typeof console.info;

  beforeEach(() => {
    _resetFrontendLogBridgeForTests();
    origError = console.error;
    origWarn = console.warn;
    origInfo = console.info;
    vi.mocked(invoke).mockClear();
    vi.mocked(invoke).mockResolvedValue(undefined);
  });

  afterEach(() => {
    console.error = origError;
    console.warn = origWarn;
    console.info = origInfo;
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

  // Issue #602: console.info must forward so the frontend xterm_mount
  // spawn-timing checkpoint reaches buildmesh.log alongside the Rust
  // SpawnTimer lines (the Rust `log_frontend` already maps level="info"
  // to `tracing::info!(target: "frontend", …)`).
  it('install patches console.info to forward and still call original', () => {
    const spy = vi.fn();
    console.info = spy;
    installFrontendLogBridge();

    console.info('spawn_timing: session=42 checkpoint=xterm_mount elapsed=17ms');

    expect(spy).toHaveBeenCalledWith(
      'spawn_timing: session=42 checkpoint=xterm_mount elapsed=17ms',
    );
    expect(invoke).toHaveBeenCalledWith('log_frontend', {
      level: 'info',
      message: 'spawn_timing: session=42 checkpoint=xterm_mount elapsed=17ms',
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

  it('does not forward credential fields from a thrown object', () => {
    installFrontendLogBridge();
    console.error('failed', {
      status: 500,
      author: 'octocat',
      apiKey: 'sk-ant-api03-DUMMYKEYEXAMPLEabcdefghijklmnop1234567890AB',
      nested: {
        deviceToken: '0123456789abcdef0123456789abcdef',
        note: 'retry',
      },
      api_keys: ['plain-secret-no-shape'],
    });

    const call = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    expect(call).toBeDefined();
    const message = (call![1] as { message: string }).message;
    expect(message).toContain('failed');
    expect(message).toContain('500');
    expect(message).toContain('octocat');
    expect(message).toContain('retry');
    expect(message).not.toContain('sk-ant-api03-DUMMYKEYEXAMPLEabcdefghijklmnop1234567890AB');
    expect(message).not.toContain('0123456789abcdef0123456789abcdef');
    expect(message).not.toContain('plain-secret-no-shape');
  });

  it('does not repeat the name and message a stack already carries', () => {
    installFrontendLogBridge();
    const err = new Error('kaboom');
    // A V8 stack opens with `Name: message`, so printing the header and then
    // the stack would print it twice and spend the stack cap on the repeat.
    err.stack = 'Error: kaboom\n    at fn (file.ts:10)';

    console.error(err);

    const call = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    const message = (call![1] as { message: string }).message;
    expect(message.match(/Error: kaboom/g)).toHaveLength(1);
    expect(message).toContain('at fn (file.ts:10)');
  });

  it('keeps a stack that does not repeat the header', () => {
    installFrontendLogBridge();
    const err = new TypeError('bad type');
    // A non-V8 stack (or a browser that trims the header) must survive whole.
    err.stack = 'frames only\n    at fn (file.ts:10)';

    console.error(err);

    const call = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    const message = (call![1] as { message: string }).message;
    expect(message).toContain('TypeError: bad type');
    expect(message).toContain('frames only');
    expect(message).toContain('at fn (file.ts:10)');
  });

  it('does not json-stringify extra fields attached to an Error', () => {
    installFrontendLogBridge();
    const err = new Error('kaboom');
    err.stack = 'Error: kaboom\n    at fn (file.ts:10)';
    (err as Error & { token: string }).token = 'ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345';

    console.error(err);

    const call = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    expect(call).toBeDefined();
    const message = (call![1] as { message: string }).message;
    expect(message).toContain('Error: kaboom');
    expect(message).toContain('at fn (file.ts:10)');
    expect(message).not.toContain('ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345');
  });

  it('redacts credential fields on an unhandledrejection object', () => {
    installFrontendLogBridge();
    const event = new Event('unhandledrejection') as PromiseRejectionEvent;
    Object.defineProperty(event, 'reason', {
      value: {
        rootToken: '0123456789abcdef0123456789abcdef',
        note: 'retry later',
      },
    });
    window.dispatchEvent(event);

    const call = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    expect(call).toBeDefined();
    const message = (call![1] as { message: string }).message;
    expect(message).toContain('unhandledrejection');
    expect(message).toContain('retry later');
    expect(message).not.toContain('0123456789abcdef0123456789abcdef');
  });

  it('redacts a free-text provider key before the length cap can split it', () => {
    installFrontendLogBridge();
    const key = 'sk-ant-api03-DUMMYKEYEXAMPLEabcdefghijklmnop1234567890AB';
    // A space keeps the token on a word boundary, which is how the masker
    // recognizes it. The key starts inside the last few characters of the cap,
    // so slicing first would leave a prefix too short to match.
    console.error(`${'x'.repeat(4079)} ${key}`);

    const call = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    expect(call).toBeDefined();
    const message = (call![1] as { message: string }).message;
    expect(message).not.toContain('sk-ant-api03');
    expect(message).not.toContain('DUMMYKEY');
    expect(message.startsWith('x')).toBe(true);
  });

  it('caps a huge stack instead of forwarding it whole', () => {
    installFrontendLogBridge();
    const err = new Error('deep');
    err.stack = `Error: deep\n${'    at frame (file.ts:1)\n'.repeat(400)}`;

    console.error(err);

    const call = vi.mocked(invoke).mock.calls.find(c => c[0] === 'log_frontend');
    expect(call).toBeDefined();
    const message = (call![1] as { message: string }).message;
    expect(message).toContain('Error: deep');
    expect(message).toContain('truncated');
    expect(message.length).toBeLessThan(err.stack.length);
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
