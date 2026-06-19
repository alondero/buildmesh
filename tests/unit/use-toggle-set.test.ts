/**
 * Tests for `useToggleSet<T>()` — issue #463.
 *
 * The hook is a thin abstraction over `useState<Set<T>>` that
 * eliminates the `useState<Set<number>>(new Set())` +
 * `toggleExpanded(n) = setExpanded(prev => ...)` pattern duplicated
 * across the Issues and PRs Probe tabs. The shape is:
 *
 *   const set = useToggleSet<number>();
 *   set.isExpanded(n)         // boolean membership check
 *   set.toggle(n)             // flip membership (add if absent, delete if present)
 *   set.clear()               // reset to empty (used by mesh/filter load effect)
 *   set.size                  // for assertions / future affordances
 *
 * Tests cover each public method independently, then a sequence that
 * mirrors the real load-effect reset (toggle a few, clear, verify
 * empty), and finally the generic-key contract with `string` keys so
 * a future caller (e.g. branch name, owner/repo pair) can adopt it
 * without re-implementing.
 */

import { describe, it, expect } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useToggleSet } from '../../src/hooks/useToggleSet';

describe('useToggleSet (#463)', () => {
  it('starts empty on first render', () => {
    const { result } = renderHook(() => useToggleSet<number>());
    expect(result.current.size).toBe(0);
    expect(result.current.isExpanded(1)).toBe(false);
    expect(result.current.isExpanded(99)).toBe(false);
  });

  it('toggle adds a previously-absent key', () => {
    const { result } = renderHook(() => useToggleSet<number>());
    act(() => result.current.toggle(7));
    expect(result.current.isExpanded(7)).toBe(true);
    expect(result.current.size).toBe(1);
  });

  it('toggle removes a previously-present key (idempotent flip)', () => {
    const { result } = renderHook(() => useToggleSet<number>());
    act(() => result.current.toggle(7));
    expect(result.current.isExpanded(7)).toBe(true);
    act(() => result.current.toggle(7));
    expect(result.current.isExpanded(7)).toBe(false);
    expect(result.current.size).toBe(0);
  });

  it('toggle keeps other keys intact when flipping one', () => {
    // Adjacent-key independence — a buggy implementation that
    // accidentally rebuilt the Set from a single key would lose the
    // rest of the membership. Pin each.
    const { result } = renderHook(() => useToggleSet<number>());
    act(() => result.current.toggle(1));
    act(() => result.current.toggle(2));
    act(() => result.current.toggle(3));
    expect(result.current.size).toBe(3);
    act(() => result.current.toggle(2));
    expect(result.current.isExpanded(1)).toBe(true);
    expect(result.current.isExpanded(2)).toBe(false);
    expect(result.current.isExpanded(3)).toBe(true);
    expect(result.current.size).toBe(2);
  });

  it('clear resets to an empty set without unmounting', () => {
    // The load effect in both tabs calls `clear()` on every mesh /
    // filter change so the prior Set's numbers don't re-open unrelated
    // rows in the new view. Pin that contract here.
    const { result } = renderHook(() => useToggleSet<number>());
    act(() => result.current.toggle(1));
    act(() => result.current.toggle(2));
    expect(result.current.size).toBe(2);

    act(() => result.current.clear());
    expect(result.current.size).toBe(0);
    expect(result.current.isExpanded(1)).toBe(false);
    expect(result.current.isExpanded(2)).toBe(false);
  });

  it('isExpanded is a pure read (does not mutate the set)', () => {
    const { result } = renderHook(() => useToggleSet<number>());
    act(() => result.current.toggle(42));
    const sizeBefore = result.current.size;
    // Read several times — the Set must stay at the same size.
    expect(result.current.isExpanded(42)).toBe(true);
    expect(result.current.isExpanded(42)).toBe(true);
    expect(result.current.isExpanded(99)).toBe(false);
    expect(result.current.size).toBe(sizeBefore);
  });

  it('works with string keys (generic over T)', () => {
    // The hook's contract is `T extends ...` so a future caller can
    // key by branch name or owner/repo. Pin the generic with strings
    // — `useState<Set<string>>` would catch a regression that hard-codes
    // `number`.
    const { result } = renderHook(() => useToggleSet<string>());
    act(() => result.current.toggle('feat/wobble'));
    expect(result.current.isExpanded('feat/wobble')).toBe(true);
    expect(result.current.isExpanded('feat/other')).toBe(false);
    act(() => result.current.toggle('feat/wobble'));
    expect(result.current.isExpanded('feat/wobble')).toBe(false);
  });
});