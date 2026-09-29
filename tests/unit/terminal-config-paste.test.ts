import { describe, expect, it } from 'vitest';
import { ignoreBracketedPasteForHarness, prefersNativeClipboardPasteForHarness } from '../../src/components/Terminal/terminalConfig';

describe('ignoreBracketedPasteForHarness', () => {
  it('is true for Command Code so xterm.js will not emit CSI 200~/201~ paste wrappers', () => {
    expect(ignoreBracketedPasteForHarness('commandcode')).toBe(true);
  });

  it('is false for other harnesses that rely on bracketed paste for multi-line prompts', () => {
    expect(ignoreBracketedPasteForHarness('anthropic')).toBe(false);
    expect(ignoreBracketedPasteForHarness('codex')).toBe(false);
    expect(ignoreBracketedPasteForHarness('opencode')).toBe(false);
    expect(ignoreBracketedPasteForHarness('claude:minimax')).toBe(false);
  });
});

describe('native clipboard paste', () => {
  it('is enabled only for Grok sharing the Windows desktop clipboard', () => {
    expect(prefersNativeClipboardPasteForHarness('grok', 'windows', true)).toBe(true);
    expect(prefersNativeClipboardPasteForHarness('grok:custom', 'windows', true)).toBe(true);
    expect(prefersNativeClipboardPasteForHarness('grok', 'wsl', true)).toBe(false);
    expect(prefersNativeClipboardPasteForHarness('grok', 'windowsinterop', false)).toBe(false);
    expect(prefersNativeClipboardPasteForHarness('grok', 'windows', false)).toBe(false);
    expect(prefersNativeClipboardPasteForHarness('grok', undefined, true)).toBe(false);
    for (const harness of ['anthropic', 'codex', 'commandcode', 'terminal', '']) {
      expect(prefersNativeClipboardPasteForHarness(harness, 'windows', true)).toBe(false);
    }
  });
});
