import { EventEmitter } from 'node:events';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { collectPageErrors } from '../../scripts/ui-shot-errors.mjs';

afterEach(() => vi.restoreAllMocks());

describe('ui-shot React warning classification', () => {
  it.each([
    'Each child in a list should have a unique "key" prop.',
    'Encountered two children with the same key, duplicate-control.',
    'React has detected a change in the order of Hooks called by HookControl.',
    'Invalid hook call. Hooks can only be called inside of the body of a function component.',
    'Rendered fewer hooks than expected. This may be caused by an accidental early return.',
    'Rendered more hooks than during the previous render.',
  ])('rejects console.warn: %s', async diagnostic => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const page = Object.assign(new EventEmitter(), { evaluate: async () => null });
    const collector = collectPageErrors(page);
    page.emit('console', { type: () => 'warning', text: () => diagnostic });
    await expect(collector.assertHealthy()).rejects.toThrow(`[React warning] ${diagnostic}`);
  });

  it('allows ordinary warnings and React text logged at info severity', async () => {
    const page = Object.assign(new EventEmitter(), { evaluate: async () => null });
    const collector = collectPageErrors(page);
    page.emit('console', { type: () => 'warning', text: () => 'ordinary warning' });
    page.emit('console', { type: () => 'log', text: () => 'Rendered fewer hooks than expected.' });
    await expect(collector.assertHealthy()).resolves.toBeUndefined();
  });
});
