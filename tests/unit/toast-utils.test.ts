import { describe, it, expect } from 'vitest';
import {
  createToastKey,
  dedupToasts,
  applyToastCap,
  TOAST_TTL_MS,
  TOAST_DEDUP_TTL_MS,
  TOAST_MAX,
  type Toast,
} from '../../src/lib/toastUtils';

// Deterministic timestamp factory so failures print legible diffs.
const t = (n: number) => 1_700_000_000_000 + n;
const make = (overrides: Partial<Toast> = {}): Toast => ({
  id: 1,
  provider: 'Worktree',
  message: "Couldn't remove worktree for foo",
  createdAt: t(0),
  ...overrides,
});

describe('createToastKey', () => {
  it('returns the same key for identical provider + message', () => {
    expect(createToastKey(make({ provider: 'Worktree', message: 'oops' })))
      .toBe(createToastKey(make({ provider: 'Worktree', message: 'oops' })));
  });

  it('returns different keys for different providers', () => {
    expect(createToastKey(make({ provider: 'Worktree', message: 'same' })))
      .not.toBe(createToastKey(make({ provider: 'System', message: 'same' })));
  });

  it('returns different keys for different messages', () => {
    expect(createToastKey(make({ message: 'a' })))
      .not.toBe(createToastKey(make({ message: 'b' })));
  });

  it('ignores id and createdAt — only provider + message matter', () => {
    const a = createToastKey(make({ id: 1, createdAt: t(0) }));
    const b = createToastKey(make({ id: 2, createdAt: t(999) }));
    expect(a).toBe(b);
  });

  it('truncates long messages consistently so the key is bounded', () => {
    const long = 'x'.repeat(500);
    const key = createToastKey(make({ message: long }));
    // Truncation point + provider prefix should be the only parts of the key.
    expect(key).toBe(`Worktree:${'x'.repeat(200)}`);
  });
});

describe('dedupToasts', () => {
  it('appends incoming when existing is empty', () => {
    const incoming = make({ id: 1 });
    expect(dedupToasts([], incoming, t(0), TOAST_DEDUP_TTL_MS)).toEqual([incoming]);
  });

  it('appends to the end when no key matches', () => {
    const first = make({ id: 1, provider: 'Worktree', message: 'a', createdAt: t(0) });
    const incoming = make({ id: 2, provider: 'System', message: 'b', createdAt: t(10) });
    const result = dedupToasts([first], incoming, t(10), TOAST_DEDUP_TTL_MS);
    expect(result).toEqual([first, incoming]);
  });

  it('replaces in place — preserving id and position — when key matches within TTL', () => {
    const existing = make({ id: 1, message: 'oops', createdAt: t(0) });
    const newer = make({ id: 2, message: 'oops', createdAt: t(5_000) });
    // Within TTL (15_000ms default).
    const result = dedupToasts([existing], newer, t(5_000), TOAST_DEDUP_TTL_MS);

    expect(result).toHaveLength(1);
    expect(result[0].id).toBe(1);          // id preserved
    expect(result[0].createdAt).toBe(t(5_000)); // createdAt bumped
  });

  it('appends (treats as new) when the matching key is past the dedup TTL', () => {
    const stale = make({ id: 1, message: 'oops', createdAt: t(0) });
    const incoming = make({ id: 2, message: 'oops', createdAt: t(20_000) });
    // 20s gap > 15s TTL — not dedup territory.
    const result = dedupToasts([stale], incoming, t(20_000), TOAST_DEDUP_TTL_MS);
    expect(result).toHaveLength(2);
    expect(result[0].id).toBe(1);
    expect(result[1].id).toBe(2);
  });

  it('replaces the matching one and keeps others in order', () => {
    const a = make({ id: 1, message: 'a', createdAt: t(0) });
    const b = make({ id: 2, message: 'b', createdAt: t(100) });
    const c = make({ id: 3, message: 'c', createdAt: t(200) });
    const dupOfB = make({ id: 99, message: 'b', createdAt: t(300) });

    const result = dedupToasts([a, b, c], dupOfB, t(300), TOAST_DEDUP_TTL_MS);

    expect(result.map((toast) => toast.id)).toEqual([1, 2, 3]);
    expect(result[1].createdAt).toBe(t(300)); // bumped, not replaced wholesale
  });

  it('returns a new array — never mutates the input', () => {
    const existing = [make({ id: 1, message: 'a', createdAt: t(0) })];
    const before = [...existing];
    const incoming = make({ id: 2, message: 'b', createdAt: t(10) });
    dedupToasts(existing, incoming, t(10), TOAST_DEDUP_TTL_MS);
    expect(existing).toEqual(before);
  });
});

describe('applyToastCap', () => {
  it('returns the input unchanged when under the cap', () => {
    const list = [make({ id: 1 }), make({ id: 2 })];
    expect(applyToastCap(list, 3)).toBe(list); // same reference — pure pass-through
  });

  it('returns the input unchanged when exactly at the cap', () => {
    const list = [make({ id: 1 }), make({ id: 2 }), make({ id: 3 })];
    expect(applyToastCap(list, 3)).toBe(list);
  });

  it('drops oldest (FIFO) when over the cap', () => {
    const a = make({ id: 1, createdAt: t(0) });
    const b = make({ id: 2, createdAt: t(10) });
    const c = make({ id: 3, createdAt: t(20) });
    const d = make({ id: 4, createdAt: t(30) });
    const result = applyToastCap([a, b, c, d], 3);
    expect(result.map((toast) => toast.id)).toEqual([2, 3, 4]);
  });

  it('handles cap of 0 by emptying the list', () => {
    expect(applyToastCap([make({ id: 1 })], 0)).toEqual([]);
  });
});

describe('constants', () => {
  it('uses sensible defaults (sanity check)', () => {
    // If any of these change, the rest of the test suite is stale.
    expect(TOAST_TTL_MS).toBe(15_000);
    expect(TOAST_DEDUP_TTL_MS).toBe(15_000);
    expect(TOAST_MAX).toBe(3);
  });
});
