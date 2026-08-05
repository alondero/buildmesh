import { describe, expect, it } from 'vitest';
import { brandFor } from '../../src/lib/brandRegistry';

describe('brandFor', () => {
  it('resolves a Proxied Spawn Option id to its provider brand', () => {
    expect(brandFor('claude:minimax')).toMatchObject({
      id: 'minimax',
      chipHex: '#6366f1',
      chipClass: 'bg-indigo-500',
    });
  });

  it('returns the same brand record for canonical and alias ids', () => {
    expect(brandFor('claude')).toBe(brandFor('anthropic'));
  });

  it('keeps Kimi identity stable across canonical and composite ids', () => {
    const kimi = brandFor('kimi');
    expect(brandFor('claude:kimi')).toBe(kimi);
    expect(kimi).toMatchObject({
      id: 'kimi',
      chipHex: '#00c4c4',
      chipClass: 'bg-cyan-500',
    });
  });

  it('returns undefined for an unregistered provider', () => {
    expect(brandFor('claude:custom-account')).toBeUndefined();
    expect(brandFor('mystery')).toBeUndefined();
  });
});
