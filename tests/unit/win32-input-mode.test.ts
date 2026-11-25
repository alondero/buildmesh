import { describe, expect, it, vi } from 'vitest';
import {
  createWin32InputModeTracker,
  encodeWin32InputModeKey,
  WIN32_SHIFT_ENTER_KEY_DOWN,
} from '../../src/lib/win32InputMode';

type CsiCallback = (params: (number | number[])[]) => boolean;

function registrationHarness() {
  const handlers = new Map<string, CsiCallback>();
  const disposers: Array<ReturnType<typeof vi.fn>> = [];
  const register = vi.fn((id: { prefix?: string; final: string }, callback: CsiCallback) => {
    handlers.set(`${id.prefix ?? ''}${id.final}`, callback);
    const dispose = vi.fn();
    disposers.push(dispose);
    return { dispose };
  });
  return { register, handlers, disposers };
}

describe('createWin32InputModeTracker', () => {
  it('tracks DECSET and DECRST 9001 without consuming other private modes', () => {
    const { register, handlers } = registrationHarness();
    const tracker = createWin32InputModeTracker(register);

    expect(tracker.enabled).toBe(false);
    expect(register).toHaveBeenCalledWith({ prefix: '?', final: 'h' }, expect.any(Function));
    expect(register).toHaveBeenCalledWith({ prefix: '?', final: 'l' }, expect.any(Function));

    expect(handlers.get('?h')!([2026])).toBe(false);
    expect(tracker.enabled).toBe(false);

    expect(handlers.get('?h')!([9001])).toBe(false);
    expect(tracker.enabled).toBe(true);

    expect(handlers.get('?l')!([9001])).toBe(false);
    expect(tracker.enabled).toBe(false);
  });

  it('handles multi-parameter mode sequences and disposes both parser registrations', () => {
    const { register, handlers, disposers } = registrationHarness();
    const tracker = createWin32InputModeTracker(register);

    handlers.get('?h')!([2026, 9001]);
    expect(tracker.enabled).toBe(true);

    tracker.dispose();
    expect(disposers).toHaveLength(2);
    expect(disposers[0]).toHaveBeenCalledOnce();
    expect(disposers[1]).toHaveBeenCalledOnce();
  });

  it('tracks mode 9001 when xterm.js parses terminal output', async () => {
    const { Terminal } = await vi.importActual<typeof import('@xterm/xterm')>('@xterm/xterm');
    const term = new Terminal();
    const tracker = createWin32InputModeTracker((id, callback) =>
      term.parser.registerCsiHandler(id, callback),
    );
    const write = (data: string) => new Promise<void>((resolve) => term.write(data, resolve));

    await write('\x1b[?9001h');
    expect(tracker.enabled).toBe(true);

    await write('\x1b[?9001l');
    expect(tracker.enabled).toBe(false);

    tracker.dispose();
    term.dispose();
  });
});

describe('encodeWin32InputModeKey', () => {
  const shiftEnter = {
    type: 'keydown',
    key: 'Enter',
    shiftKey: true,
    ctrlKey: false,
    altKey: false,
    metaKey: false,
  };

  it('encodes Shift+Enter as the Windows Enter key record only while mode 9001 is enabled', () => {
    expect(encodeWin32InputModeKey(shiftEnter, true)).toBe(WIN32_SHIFT_ENTER_KEY_DOWN);
    expect(encodeWin32InputModeKey(shiftEnter, true)).toBe('\x1b[13;28;13;1;16;1_');
    expect(encodeWin32InputModeKey(shiftEnter, false)).toBeNull();
  });

  it('leaves plain Enter, keyup, and additional-modifier chords on the existing xterm path', () => {
    expect(encodeWin32InputModeKey({ ...shiftEnter, shiftKey: false }, true)).toBeNull();
    expect(encodeWin32InputModeKey({ ...shiftEnter, type: 'keyup' }, true)).toBeNull();
    expect(encodeWin32InputModeKey({ ...shiftEnter, ctrlKey: true }, true)).toBeNull();
    expect(encodeWin32InputModeKey({ ...shiftEnter, altKey: true }, true)).toBeNull();
    expect(encodeWin32InputModeKey({ ...shiftEnter, metaKey: true }, true)).toBeNull();
    expect(encodeWin32InputModeKey({ ...shiftEnter, key: 'a' }, true)).toBeNull();
  });
});
