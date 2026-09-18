import { describe, expect, it } from 'vitest';
import { ignoreBracketedPasteForHarness } from '../../src/components/Terminal/terminalConfig';

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
