import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { useToastStore, addToast, dismissToast } from '../../src/stores/toastStore';
import { TOAST_DEDUP_TTL_MS, TOAST_MAX, TOAST_TTL_MS, type Toast } from '../../src/lib/toastUtils';

// Issue #1001 — store-level tests for the lifted `addToast` / `dismissToast`
// Zustand surface. The pure helpers (`dedupToasts`, `applyToastCap`) are
// already covered by `toast-utils.test.ts`; these tests pin the store's
// behavior end-to-end, including the imperative wrappers used by non-React
// callers (event listeners, store actions, naming-backend callback).
//
// We freeze `Date.now()` to make dedup and cap assertions deterministic.
// Two synchronous `addToast` calls in the same test would otherwise share
// a millisecond, which would scramble id/createdAt assertions.

const FROZEN_NOW = 1_700_000_000_000;

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(FROZEN_NOW);
  // Reset only the data — keep the action functions installed.
  useToastStore.setState({ toasts: [] });
});

afterEach(() => {
  vi.useRealTimers();
});

describe('useToastStore', () => {
  it('starts with an empty toast list', () => {
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  describe('addToast', () => {
    it('pushes to an empty list (one toast, default severity "error")', () => {
      useToastStore.getState().addToast('System', 'first failure');
      const toasts = useToastStore.getState().toasts;
      expect(toasts).toHaveLength(1);
      expect(toasts[0]).toMatchObject({
        provider: 'System',
        message: 'first failure',
        severity: 'error',
        createdAt: FROZEN_NOW,
      });
      expect(typeof toasts[0].id).toBe('number');
    });

    it('preserves an explicit "warning" severity', () => {
      useToastStore.getState().addToast('Sync', 'drift detected', 'warning');
      expect(useToastStore.getState().toasts[0].severity).toBe('warning');
    });

    it('dedupes a matching provider+message within the dedup TTL', () => {
      useToastStore.getState().addToast('System', 'first failure');
      const firstId = useToastStore.getState().toasts[0].id;

      // Advance 1s — still well within TOAST_DEDUP_TTL_MS.
      vi.setSystemTime(FROZEN_NOW + 1_000);
      useToastStore.getState().addToast('System', 'first failure');

      const toasts = useToastStore.getState().toasts;
      expect(toasts).toHaveLength(1);
      // id preserved (key React-list stability), createdAt bumped.
      expect(toasts[0].id).toBe(firstId);
      expect(toasts[0].createdAt).toBe(FROZEN_NOW + 1_000);
    });

    it('appends a fresh toast when the dedup TTL has elapsed', () => {
      useToastStore.getState().addToast('System', 'first failure');

      // Advance past the dedup TTL — a second call is a new toast, not a
      // refresh.
      vi.setSystemTime(FROZEN_NOW + TOAST_DEDUP_TTL_MS + 1);
      useToastStore.getState().addToast('System', 'first failure');

      expect(useToastStore.getState().toasts).toHaveLength(2);
    });

    it('caps to TOAST_MAX entries by dropping the oldest (FIFO)', () => {
      for (let i = 0; i < TOAST_MAX; i++) {
        vi.setSystemTime(FROZEN_NOW + i * 1_000);
        useToastStore.getState().addToast('System', `msg ${i}`);
      }
      expect(useToastStore.getState().toasts).toHaveLength(TOAST_MAX);

      // One more — drops the oldest (msg 0), keeps the latest TOAST_MAX.
      vi.setSystemTime(FROZEN_NOW + TOAST_MAX * 1_000);
      useToastStore.getState().addToast('System', `msg ${TOAST_MAX}`);

      const messages = useToastStore.getState().toasts.map((t) => t.message);
      expect(messages).toEqual(['msg 1', 'msg 2', `msg ${TOAST_MAX}`]);
    });

    it('treats different providers as distinct toasts (no cross-provider dedup)', () => {
      useToastStore.getState().addToast('System', 'same message');
      useToastStore.getState().addToast('GitHub', 'same message');
      expect(useToastStore.getState().toasts).toHaveLength(2);
    });
  });

  describe('dismissToast', () => {
    it('removes the toast with the matching id', () => {
      // Advance time between adds so the two toasts get distinct ids —
      // real-world callers spread adds across renders / events, but
      // `Date.now()`-based ids collapse under synchronous back-to-back
      // calls. That's a pre-existing limitation of the id scheme (not
      // introduced by #1001); see the auto-dismiss interval comment for
      // the same constraint. The functional contract being tested here
      // is "dismiss by id removes the matching toast", which holds once
      // ids are distinct.
      vi.setSystemTime(FROZEN_NOW);
      useToastStore.getState().addToast('System', 'a');
      vi.setSystemTime(FROZEN_NOW + 1);
      useToastStore.getState().addToast('GitHub', 'b');
      const [first, second] = useToastStore.getState().toasts;

      useToastStore.getState().dismissToast(first.id);

      const remaining = useToastStore.getState().toasts;
      expect(remaining).toHaveLength(1);
      expect(remaining[0].id).toBe(second.id);
    });

    it('is a no-op when the id is not present', () => {
      useToastStore.getState().addToast('System', 'a');
      useToastStore.getState().dismissToast(999_999);
      expect(useToastStore.getState().toasts).toHaveLength(1);
    });
  });

  describe('dismissExpired', () => {
    // Backs the auto-dismiss interval in App.tsx. Functional `set((s) =>
    // ...)` so a dedup-refresh of `createdAt` during the tick is
    // respected — the alternative (filtering a closed-over `toasts`
    // reference) would expire a toast whose TTL was just reset.

    it('drops toasts older than TOAST_TTL_MS', () => {
      vi.setSystemTime(FROZEN_NOW);
      useToastStore.getState().addToast('System', 'old');
      // Different provider + 200ms gap so the two toasts have distinct ids
      // (Date.now()-based ids collapse under synchronous back-to-back adds —
      // see dismissToast test for the same constraint) AND so 'fresh'
      // sits comfortably inside its own TTL window when 'old' expires.
      vi.setSystemTime(FROZEN_NOW + 200);
      useToastStore.getState().addToast('GitHub', 'fresh');

      // 'old' (T=0) is past TTL at T=15_100. 'fresh' (T=200) is still
      // inside TTL (15_100 - 200 = 14_900 < 15_000).
      useToastStore.getState().dismissExpired(FROZEN_NOW + 15_100);

      const messages = useToastStore.getState().toasts.map((t) => t.message);
      expect(messages).toEqual(['fresh']);
    });

    it('keeps a dedup-refreshed toast past the original createdAt', () => {
      // The original toast is added at T=0 with TTL=15s. A second
      // addToast (same provider+message) at T=14s dedupes and bumps
      // createdAt to T=14s. Calling dismissExpired at T=20s would
      // expire the original createdAt but must NOT expire the
      // refreshed one — its effective TTL ends at T=14s+15s=T=29s.
      useToastStore.getState().addToast('System', 'oops');
      vi.setSystemTime(FROZEN_NOW + 14_000);
      useToastStore.getState().addToast('System', 'oops');

      useToastStore.getState().dismissExpired(FROZEN_NOW + 20_000);

      expect(useToastStore.getState().toasts).toHaveLength(1);
    });

    it('is a no-op when no toasts have expired', () => {
      useToastStore.getState().addToast('System', 'a');
      useToastStore.getState().dismissExpired(FROZEN_NOW + 1_000);
      expect(useToastStore.getState().toasts).toHaveLength(1);
    });
  });

  describe('imperative wrappers', () => {
    // The wrappers exist so non-React callers (event listeners, store
    // actions, naming-backend callback) don't need to write
    // `useToastStore.getState().addToast(...)` everywhere. They reach the
    // store via `.getState()` under the hood — same path as the hook
    // subscribers. This test pins that contract: the wrapper mutates the
    // store, the store subscribers see the new array. A separate
    // "wrapper forwards to action" test would be a low-value
    // implementation assertion; behavior at the store boundary is what
    // callers actually depend on.

    it('addToast(...) reaches the store', () => {
      addToast('System', 'via wrapper');
      const toasts = useToastStore.getState().toasts;
      expect(toasts).toHaveLength(1);
      expect(toasts[0]).toMatchObject({
        provider: 'System',
        message: 'via wrapper',
        severity: 'error',
      });
    });

    it('dismissToast(id) reaches the store', () => {
      addToast('System', 'x');
      const id = useToastStore.getState().toasts[0].id;
      dismissToast(id);
      expect(useToastStore.getState().toasts).toEqual([]);
    });

    it('preserves explicit severity through the wrapper', () => {
      addToast('Sync', 'heads-up', 'warning');
      expect(useToastStore.getState().toasts[0].severity).toBe('warning');
    });

    it('the store action and the wrapper share identity (no double-write)', () => {
      // Both paths point at the same store action. A regression that
      // double-routes the wrapper would manifest here as two toasts.
      useToastStore.getState().addToast('System', 'store path');
      addToast('GitHub', 'wrapper path');
      const providers = useToastStore.getState().toasts.map((t: Toast) => t.provider);
      expect(providers).toEqual(['System', 'GitHub']);
    });
  });
});
