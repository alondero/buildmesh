// Pure helpers for the global error-toast stack in App.tsx.
//
// `dedupToasts` and `applyToastCap` are pure so App.tsx's `setToasts`
// updaters stay trivially correct under React 19's StrictMode (double
// invocation in dev) — there is no shared mutable state, and the
// incoming `now` is passed in rather than read from `Date.now()` so
// tests can be deterministic.

export const TOAST_TTL_MS = 15_000;
export const TOAST_DEDUP_TTL_MS = 15_000;
export const TOAST_MAX = 3;

// Bounded so a freakishly long error string can't bloat the key (and
// the dedup-map we'd build off it) without bound.
const KEY_MESSAGE_MAX = 200;

export interface Toast {
  id: number;
  provider: string;
  message: string;
  createdAt: number;
}

export function createToastKey(toast: Pick<Toast, 'provider' | 'message'>): string {
  return `${toast.provider}:${toast.message.slice(0, KEY_MESSAGE_MAX)}`;
}

// Replaces an existing toast with the same key when it's still on
// screen (within `dedupTtlMs`); otherwise appends. The replace keeps
// the id so the React list stays stable and preserves visual
// position; only `createdAt` is bumped, which refreshes the auto-
// dismiss timer.
export function dedupToasts(
  existing: Toast[],
  incoming: Toast,
  now: number,
  dedupTtlMs: number,
): Toast[] {
  const key = createToastKey(incoming);
  const idx = existing.findIndex(
    (t) => createToastKey(t) === key && now - t.createdAt <= dedupTtlMs,
  );
  if (idx === -1) {
    return [...existing, incoming];
  }
  const next = existing.slice();
  next[idx] = { ...existing[idx], createdAt: incoming.createdAt };
  return next;
}

// FIFO: the oldest toasts are dropped first when the stack grows
// past `max`. Returns the same array reference when already at or
// under the cap (caller-friendly for React identity stability).
export function applyToastCap(merged: Toast[], max: number): Toast[] {
  if (merged.length <= max) return merged;
  return merged.slice(merged.length - max);
}
