import type { NotificationAction } from './notifications';

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

// 'error' = something failed and needs attention (provider errors, store
// errors). 'warning' = a non-blocking heads-up (sync drift, worktree cleanup
// retry, resume hiccups). The distinction drives the toast's colour and label
// so a benign sync notice no longer shouts "ERROR" in red.
export type ToastSeverity = 'error' | 'warning' | 'success' | 'info';

export interface Toast {
  id: number;
  provider: string;
  message: string;
  createdAt: number;
  severity?: ToastSeverity;
  action?: NotificationAction;
}

export function createToastKey(toast: Pick<Toast, 'provider' | 'message' | 'action'>): string {
  const target = toast.action?.kind === 'agent-input' ? `:agent:${toast.action.nodeId}` : '';
  return `${toast.provider}:${toast.message.slice(0, KEY_MESSAGE_MAX)}${target}`;
}

// Replaces an existing toast with the same key when it's still on
// screen (within `dedupTtlMs`); otherwise appends. The replace keeps
// the id so the React list stays stable and preserves visual
// position. The incoming message, time, severity and action replace the
// payload, refreshing both the auto-dismiss timer and displayed details.
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
  // Retain list identity while refreshing the complete payload, including
  // messages whose tails differ beyond the bounded key and updated actions.
  const next = existing.slice();
  next[idx] = { ...incoming, id: existing[idx].id };
  return next;
}

// FIFO: the oldest toasts are dropped first when the stack grows
// past `max`. Returns the same array reference when already at or
// under the cap (caller-friendly for React identity stability).
export function applyToastCap(merged: Toast[], max: number): Toast[] {
  if (merged.length <= max) return merged;
  return merged.slice(merged.length - max);
}
