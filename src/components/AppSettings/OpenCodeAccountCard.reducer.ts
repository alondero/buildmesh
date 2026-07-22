import type { OpenCodeWorkspace } from '../../types/generated/OpenCodeWorkspace';
import type { OpenCodeDeviceCodeStatus } from '../../types/generated/OpenCodeDeviceCodeStatus';
import type { OpenCodeTokenResponse } from '../../types/generated/OpenCodeTokenResponse';

/**
 * State machine for the OpenCode Console OAuth Device Flow (issue #969).
 * Driven by `useReducer` from `OpenCodeAccountCard.tsx`; the reducer lives
 * in a sibling file so it can be unit-tested without importing React.
 *
 * Transitions (all driven by side-effectful [`Command`] dispatches, never by
 * pure state derivation):
 *
 *   signedOut ─ START_REQUESTED → signedOut
 *     └─START_SUCCEEDED → awaitingActivation
 *     └─START_FAILED    → error
 *
 *   awaitingActivation ─ POLL_RESULT.Pending      → awaitingActivation
 *                    ─ POLL_RESULT.SlowDown       → awaitingActivation (interval updated)
 *                    ─ POLL_RESULT.Success        → awaitingActivation (effect needs workspaces)
 *                    ─ POLL_RESULT.CodeExpired    → error
 *                    ─ POLL_RESULT.AccessDenied   → error
 *                    ─ POLL_RESULT.Error          → error
 *                    ─ SIGNED_IN_FROM_TOKEN       → signedIn
 *
 *   signedIn  ─ WORKSPACE_CHOSEN    → signedIn
 *           ─ SIGNOUT_REQUESTED     → signedOut (optimistic)
 *           ─ SIGNOUT_FAILED        → signedIn (rollback; effect captures previous values)
 */

export type State =
  | { kind: 'signedOut' }
  | {
      kind: 'awaitingActivation';
      deviceCode: string;
      userCode: string;
      verificationUri: string;
      intervalSecs: number;
      expiresAtMs: number;
      /** Immutable window length captured at `start_device_flow_console`
       *  time. The polling IPC sends this verbatim each tick — NOT a
       *  per-tick countdown — so the Rust gate stays monotonic across the
       *  full window. Pre-fix (#1010) the field carried a countdown that
       *  caused the gate to fire at the halfway point. */
      originalExpiresInSecs: number;
      startedAtMs: number;
    }
  | {
      kind: 'signedIn';
      workspace: OpenCodeWorkspace;
      workspaces: OpenCodeWorkspace[];
      accessTokenExpiresAtMs: number;
    }
  | { kind: 'error'; message: string };

export type Action =
  | { type: 'START_REQUESTED' }
  | {
      type: 'START_SUCCEEDED';
      deviceCode: string;
      userCode: string;
      verificationUri: string;
      intervalSecs: number;
      /** Renamed from `expiresInSecs` for issue #1010: this is the
       *  ORIGINAL window length captured at dance-start, not a per-tick
       *  countdown. The reducer stores it verbatim and the IPC sends it
       *  verbatim each tick. */
      originalExpiresInSecs: number;
    }
  | { type: 'START_FAILED'; message: string }
  | { type: 'POLL_RESULT'; status: OpenCodeDeviceCodeStatus }
  | {
      type: 'SIGNED_IN_FROM_TOKEN';
      workspaces: OpenCodeWorkspace[];
      accessTokenExpiresAtMs: number;
    }
  | { type: 'WORKSPACE_CHOSEN'; workspace: OpenCodeWorkspace }
  | { type: 'SIGNOUT_REQUESTED' }
  | { type: 'SIGNOUT_SUCCEEDED' }
  // `previousWorkspace` + `previousWorkspaces` + `previousExpiresAtMs` lets the
  // component dispatch this without first re-reading the current state — pure
  // reducers can't access "the state we're about to replace". The component
  // captures the captured snapshot before dispatching SIGNOUT_REQUESTED.
  | {
      type: 'SIGNOUT_FAILED';
      message: string;
      previousWorkspace: OpenCodeWorkspace;
      previousWorkspaces: OpenCodeWorkspace[];
      previousExpiresAtMs: number;
    };

/**
 * Pure reducer. Every transition is exhaustive over `Action.type` so a
 * `never`-style check at the bottom catches typos when an action variant is
 * added without a matching branch.
 */
export function opencodeAccountReducer(state: State, action: Action): State {
  switch (action.type) {
    case 'START_REQUESTED':
      // No-op: the click handler triggers an effect that calls
      // `startOpencodeDeviceFlowConsole`; the next dispatch is
      // START_SUCCEEDED or START_FAILED. Keeping the click handler explicit
      // (rather than mutating `busy: true` here) means the card's only
      // "loading" affordances live in the IPC-level effect.
      return state;

    case 'START_SUCCEEDED': {
      // The component effect also calls `openUrl(verificationUri)` on
      // transition — the browser-open is imperative, not part of this pure
      // reducer. `expiresAtMs` is computed at dispatch time so a slow React
      // render can't cause the polling loop to drift outside the device
      // code's window. `originalExpiresInSecs` is captured verbatim for the
      // IPC to send unchanged each tick — see #1010 for why a per-tick
      // countdown breaks the Rust expiry gate.
      if (state.kind !== 'signedOut') return state;
      const now = Date.now();
      return {
        kind: 'awaitingActivation',
        deviceCode: action.deviceCode,
        userCode: action.userCode,
        verificationUri: action.verificationUri,
        intervalSecs: action.intervalSecs,
        expiresAtMs: now + action.originalExpiresInSecs * 1000,
        originalExpiresInSecs: action.originalExpiresInSecs,
        startedAtMs: now,
      };
    }

    case 'START_FAILED':
      // Accepted from either `signedOut` (start failed) or `awaitingActivation`
      // (user clicked cancel — there's no explicit cancel, but a failure
      // here means we abandon the dance). `error` is terminal-by-design with
      // a "retry" button that re-dispatches START_REQUESTED after the user
      // confirms.
      return { kind: 'error', message: action.message };

    case 'POLL_RESULT': {
      if (state.kind !== 'awaitingActivation') return state;
      const status = action.status;
      switch (status.kind) {
        case 'pending':
          // No-op: keep the same interval; the effect re-dispatches after
          // `state.intervalSecs * 1000` ms.
          return state;
        case 'slow_down':
          // RFC 8628 §3.5: the server asks us to ease off; bump the
          // interval but stay here.
          return {
            ...state,
            intervalSecs: status.new_interval_secs,
          };
        case 'success': {
          // The reducer can't call `listOpencodeWorkspaces` — it stays here,
          // and the component's effect dispatches SIGNED_IN_FROM_TOKEN after
          // the IPC returns with the workspace list. Keeping the transition
          // in the reducer (not the effect) means the "we have a token" UI
          // is gated on workspace enumeration completing, which prevents a
          // signedIn render from racing with the picker not yet having data.
          return state;
        }
        case 'code_expired':
        case 'access_denied':
        case 'error':
          return { kind: 'error', message: errorMessageFor(status) };
      }
    }

    case 'SIGNED_IN_FROM_TOKEN': {
      // Effect-driven transition. The reducer is invoked from `awaitingActivation`
      // (the dance just succeeded) — picker-list succeeds, first workspace
      // becomes the active selection. An empty `workspaces` list is defensively
      // allowed (single-workspace accounts with no org API surface leave the
      // picker as a label, not a dropdown).
      if (state.kind !== 'awaitingActivation') return state;
      const first = action.workspaces[0];
      if (!first) {
        // No workspaces at all (rare — but possible for a brand-new OAuth
        // scope). Surface as `error` so the user can retry, rather than
        // signedIn with no workspace to bind to.
        return {
          kind: 'error',
          message: 'No workspaces available for this account. Please retry.',
        };
      }
      return {
        kind: 'signedIn',
        workspace: first,
        workspaces: action.workspaces,
        accessTokenExpiresAtMs: action.accessTokenExpiresAtMs,
      };
    }

    case 'WORKSPACE_CHOSEN':
      // Optimistic; no IPC today (workspace switching is a pure visual
      // selection since the access_token's workspace_id is bound at sign-in
      // time and a future per-workspace-bound token is a separate ticket).
      if (state.kind !== 'signedIn') return state;
      return { ...state, workspace: action.workspace };

    case 'SIGNOUT_REQUESTED':
      if (state.kind !== 'signedIn') return state;
      // Optimistic flip; effect fires `revokeOpencodeConsole`. On success
      // dispatch SIGNOUT_SUCCEEDED (no-op); on failure dispatch
      // SIGNOUT_FAILED with the captured snapshot to roll back.
      return { kind: 'signedOut' };

    case 'SIGNOUT_SUCCEEDED':
      // No-op; the optimistic SIGNOUT_REQUESTED already transitioned us
      // to signedOut. Dispatching the success confirms the revoke so a
      // future "what if revoke failed silently" probe has an anchor.
      return state;

    case 'SIGNOUT_FAILED':
      // Rollback to the previous signedIn state. The captured
      // `previousWorkspace` + `previousWorkspaces` + `previousExpiresAtMs`
      // are mandatory args — without them the reducer can't reconstruct
      // the prior state. If the rollback itself is signedIn (i.e. we were
      // in awaitingActivation or signedOut when the revoke resolved),
      // preserve the failed-toast by going to error (the user's intent
      // was revoked; show them something is wrong).
      if (state.kind !== 'signedOut') return state;
      return {
        kind: 'signedIn',
        workspace: action.previousWorkspace,
        workspaces: action.previousWorkspaces,
        accessTokenExpiresAtMs: action.previousExpiresAtMs,
      };
  }
}

/** Single source of truth for how an `OpenCodeDeviceCodeStatus` error variant
 *  surfaces to the user. Keeps the `POLL_RESULT` switch tight. */
function errorMessageFor(status: Exclude<
  OpenCodeDeviceCodeStatus,
  { kind: 'pending' } | { kind: 'slow_down' } | { kind: 'success' }
>): string {
  switch (status.kind) {
    case 'code_expired':
      return 'OpenCode sign-in timed out. Please start the dance again.';
    case 'access_denied':
      return 'OpenCode sign-in was denied at the consent prompt.';
    case 'error':
      return `OpenCode OAuth failed: ${status.message}`;
  }
}

/**
 * Tiny ergonomic wrapper used by the component for `START_REQUESTED`'s
 * failure arm: takes an `unknown` thrown value (matching Tauri IPC error
 * shape) and reduces it to the `message` field. Mirrors `formatError` in
 * `lib/errorUtils.ts` but lives next to the reducer to avoid a cross-module
 * import for one helper.
 */
export function errorMessageFromUnknown(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === 'string') return err;
  try {
    return JSON.stringify(err);
  } catch {
    return 'Unknown error';
  }
}

/** Re-export so callers don't have to import `OpenCodeTokenResponse` from
 *  two places. Currently unused; kept for symmetry with the action type. */
export type { OpenCodeTokenResponse };
