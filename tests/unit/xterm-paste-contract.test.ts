import { describe, expect, it, vi } from 'vitest';

// Most terminal tests use the shared xterm mock. This contract test exercises
// the pinned xterm implementation itself because paste event granularity is
// the behavior under investigation.
vi.unmock('@xterm/xterm');

describe('xterm paste contract', () => {
  it('emits a multiline paste as one data event', async () => {
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn(() => ({
        matches: false,
        addListener: vi.fn(),
        removeListener: vi.fn(),
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
      })),
    });
    const { Terminal } = await import('@xterm/xterm');
    const term = new Terminal();
    const container = document.createElement('div');
    document.body.appendChild(container);
    term.open(container);
    const chunks: string[] = [];
    term.onData(data => chunks.push(data));
    const paste = 'first line\n' + 'pasted line\n'.repeat(200) + 'last line';

    term.paste(paste);

    expect(chunks).toEqual([paste.replace(/\n/g, '\r')]);
  });
});
