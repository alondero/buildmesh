import { describe, it, expect, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  quotePathIfNeeded,
  resolveDropText,
  pasteDropPaths,
  nodeIdFromPoint,
} from '../../src/lib/fileDropPaste';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
  Channel: class Channel {
    onmessage = (_message: unknown) => {};
    constructor(handler?: (message: unknown) => void) {
      if (handler) this.onmessage = handler;
    }
  },
}));

const mockedInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockedInvoke.mockReset();
  // to_host_path is a no-op for native paths in these tests.
  mockedInvoke.mockImplementation((_cmd, args) =>
    Promise.resolve((args as { path: string }).path),
  );
});

describe('quotePathIfNeeded', () => {
  it('leaves space-free paths untouched', () => {
    expect(quotePathIfNeeded('C:\\Users\\adam\\file.txt')).toBe('C:\\Users\\adam\\file.txt');
  });

  it('quotes paths containing spaces', () => {
    expect(quotePathIfNeeded('C:\\My Docs\\file.txt')).toBe('"C:\\My Docs\\file.txt"');
  });
});

describe('resolveDropText', () => {
  it('routes each path through to_host_path', async () => {
    await resolveDropText(['/mnt/c/Users/adam/file.txt']);
    expect(mockedInvoke).toHaveBeenCalledWith('to_host_path', { path: '/mnt/c/Users/adam/file.txt' });
  });

  it('joins multiple paths with a space and quotes those with spaces', async () => {
    const text = await resolveDropText(['C:\\a.txt', 'C:\\My Files\\b.txt']);
    expect(text).toBe('C:\\a.txt "C:\\My Files\\b.txt"');
  });

  it('drops empty host paths', async () => {
    mockedInvoke.mockResolvedValueOnce('');
    mockedInvoke.mockResolvedValueOnce('C:\\b.txt');
    const text = await resolveDropText(['bad', 'good']);
    expect(text).toBe('C:\\b.txt');
  });
});

describe('pasteDropPaths', () => {
  it('pastes the resolved absolute path into the terminal and focuses it', async () => {
    const target = { paste: vi.fn(), focus: vi.fn() };
    await pasteDropPaths(target, ['C:\\Users\\adam\\file.txt']);
    // The regression: a dropped file must be *pasted* (reaches the PTY via onData),
    // not written to the display buffer, and must carry the absolute path.
    expect(target.paste).toHaveBeenCalledWith('C:\\Users\\adam\\file.txt');
    expect(target.focus).toHaveBeenCalled();
  });

  it('does nothing when no usable path resolves', async () => {
    mockedInvoke.mockResolvedValue('');
    const target = { paste: vi.fn(), focus: vi.fn() };
    await pasteDropPaths(target, ['']);
    expect(target.paste).not.toHaveBeenCalled();
  });
});

describe('nodeIdFromPoint', () => {
  // jsdom does not implement elementFromPoint, so assign a stub directly.
  const stubElementFromPoint = (el: Element | null) => {
    document.elementFromPoint = () => el;
  };

  it('returns the node id of the container under the point', () => {
    const host = document.createElement('div');
    host.dataset.nodeId = '7';
    const child = document.createElement('span');
    host.appendChild(child);
    stubElementFromPoint(child);
    expect(nodeIdFromPoint(10, 10)).toBe(7);
  });

  it('returns null when the point is not over a terminal', () => {
    stubElementFromPoint(document.createElement('div'));
    expect(nodeIdFromPoint(10, 10)).toBeNull();
  });
});
