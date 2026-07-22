// Issue #1001 — lift `addToast` from a local App.tsx `useCallback` into a
// shared Zustand store so any component (not just App.tsx) can surface a
// user-visible error toast. Before this extraction the only error-toast
// surface in the app was local to App.tsx, which forced:
//   - the Issues Probe's trigger-label toggle (#980) to invent inline
//     error state below the badge as a workaround, and
//   - `agentNodeStore.deleteAgentNode` (#645) to silently swallow the
//     rejection into a `state.error` Zustand field no UI watches.
//
// The pure helpers (`dedupToasts`, `applyToastCap`, `TOAST_*` constants)
// stay in `src/lib/toastUtils.ts` so the 17 unit tests covering them
// continue to assert against the same surface, and so the auto-dismiss /
// dedup semantics remain trivially correct under React 19 StrictMode
// (no shared mutable state, `now` is passed in by the caller).
//
// The imperative `addToast` / `dismissToast` wrappers below exist for
// non-React callers — event listeners, store actions, naming-backend
// callback. They reach the store via `.getState()` under the hood,
// mirroring the `requestWorktreeCloseAction` pattern at
// `src/stores/worktreeClosePromptStore.ts:37-42`. React components can
// either subscribe with `useToastStore((s) => s.toasts)` (for rendering)
// or call the wrappers directly (for fire-and-forget).

import { create } from 'zustand';
import {
  applyToastCap,
  dedupToasts,
  TOAST_DEDUP_TTL_MS,
  TOAST_MAX,
  TOAST_TTL_MS,
  type Toast,
  type ToastSeverity,
} from '../lib/toastUtils';

interface ToastState {
  toasts: Toast[];
  addToast: (provider: string, message: string, severity?: ToastSeverity) => void;
  dismissToast: (id: number) => void;
  // Drop every toast whose `createdAt` is older than `TOAST_TTL_MS` relative
  // to `now`. The interval in App.tsx calls this on a 1s tick; the
  // functional update ensures a dedup-refresh of `createdAt` during the
  // tick is respected (closing over `toasts` would expire a toast whose
  // TTL was just reset). The action lives in the store so App.tsx stays
  // out of the store's data — same convention as every other cross-store
  // call in the codebase (`.getState().action(...)`).
  dismissExpired: (now: number) => void;
}

let nextToastId = 0;

export const useToastStore = create<ToastState>((set) => ({
  toasts: [],
  addToast: (provider, message, severity = 'error') => {
    const now = Date.now();
    // Functional `set((state) => ...)` is atomic against the current store
    // snapshot — important under React 19 StrictMode's double-invocation
    // and for any future caller that calls `addToast` twice in the same
    // tick (the second call would otherwise race against the first). `now`
    // is computed once above so createdAt and the dedup comparison reference
    // the same timestamp. Identity uses a monotonic counter because multiple
    // calls can share the same millisecond.
    set((state) => ({
      toasts: applyToastCap(
        dedupToasts(
          state.toasts,
          { id: ++nextToastId, provider, message, createdAt: now, severity },
          now,
          TOAST_DEDUP_TTL_MS,
        ),
        TOAST_MAX,
      ),
    }));
  },
  dismissToast: (id) =>
    set((state) => ({
      toasts: state.toasts.filter((t) => t.id !== id),
    })),
  dismissExpired: (now) =>
    set((state) => ({
      toasts: state.toasts.filter((t) => now - t.createdAt < TOAST_TTL_MS),
    })),
}));

// Imperative wrappers — non-React callers (event listeners, store
// actions, naming-backend callback) reach the store via `.getState()`.
// The default severity is applied in the store action so it lives in
// exactly one place; the wrapper just forwards `severity` unchanged
// (undefined falls through to the store's `= 'error'`).
export function addToast(
  provider: string,
  message: string,
  severity?: ToastSeverity,
): void {
  useToastStore.getState().addToast(provider, message, severity);
}

export function dismissToast(id: number): void {
  useToastStore.getState().dismissToast(id);
}
