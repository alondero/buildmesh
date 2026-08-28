import type { OpenCodeWorkspace } from '../../types/generated/OpenCodeWorkspace';
import type { OpenCodeDeviceCodeStatus } from '../../types/generated/OpenCodeDeviceCodeStatus';
import type { OpenCodeTokenResponse } from '../../types/generated/OpenCodeTokenResponse';
import type { OpenCodeConsoleStatus } from '../../types/generated/OpenCodeConsoleStatus';

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
 *     └─STATUS_FETCHED  → signedIn | signedInExpired | signedOut (no-op)
 *
 *   error | signedInExpired ─ START_REQUESTED → signedOut (recovery reset,
 *     so the subsequent START_SUCCEEDED lands cleanly in awaitingActivation;
 *     START_REQUESTED is still a no-op from signedOut/awaitingActivation/
 *     signedIn — the user can't re-fire mid-dance)
 *
 *   awaitingActivation ─ POLL_RESULT.Pending      → awaitingActivation
 *                    ─ POLL_RESULT.SlowDown       → awaitingActivation (interval updated)
 *                    ─ POLL_RESULT.Success        → awaitingActivation (effect needs workspaces)
 *                    ─ POLL_RESULT.CodeExpired    → error
 *                    ─ POLL_RESULT.AccessDenied   → error
 *                    ─ POLL_RESULT.Error          → error
 *                    ─ SIGNED_IN_FROM_TOKEN       → signedIn
 *
 *   signedIn  ─ WORKSPACE_CHOSEN_PENDING → signedIn (optimistic flip)
 *           ─ WORKSPACE_CHOSEN_CONFIRMED → signedIn (clear pending)
 *           ─ WORKSPACE_CHOSEN_FAILED   → signedIn (rollback to previousWorkspace)
 *           ─ SIGNOUT_REQUESTED         → signedOut (optimistic)
 *           ─ SIGNOUT_FAILED            → signedIn (rollback; effect captures previous values)
 *
 *   signedInExpired — same transitions as `signedIn` for the
 *     picker (PENDING/CONFIRMED/FAILED) and for SIGNOUT_REQUESTED
 *     (optimistic flip to signedOut). The expiry banner is a UI-only
 *     hint; the state machine otherwise mirrors `signedIn`.
 *
 *   signedInExpired (returned by STATUS_FETCHED) — same UI affordances as
 *   `signedIn` plus a "Session expired" hint; transitions to `signedOut`
 *   on `SIGNOUT_REQUESTED` and back to `signedIn` if the user re-dances
 *   successfully.
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
      /** Set while `set_opencode_console_workspace` is in flight; the
       *  dropdown is disabled so the user can't fire a second switch
       *  before the first resolves. Cleared on CONFIRMED / FAILED. */
      pendingWorkspaceSwitch?: OpenCodeWorkspace;
    }
  | {
      kind: 'signedInExpired';
      workspace: OpenCodeWorkspace;
      workspaces: OpenCodeWorkspace[];
      accessTokenExpiresAtMs: number;
      /** Set while `set_opencode_console_workspace` is in flight; the
       *  dropdown is disabled so the user can't fire a second switch
       *  before the first resolves. Cleared on CONFIRMED / FAILED. */
      pendingWorkspaceSwitch?: OpenCodeWorkspace;
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
  /** On-mount read of the persisted credential. Restores `signedIn`
   *  without re-running the dance when the credential is fresh, or
   *  `signedInExpired` when the `expires_at` is in the past. */
  | { type: 'STATUS_FETCHED'; status: OpenCodeConsoleStatus }
  | { type: 'WORKSPACE_CHOSEN_PENDING'; workspace: OpenCodeWorkspace }
  | { type: 'WORKSPACE_CHOSEN_CONFIRMED' }
  | {
      type: 'WORKSPACE_CHOSEN_FAILED';
      previousWorkspace: OpenCodeWorkspace;
      message: string;
    }
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
      // From `error` and `signedInExpired`: reset to `signedOut` so
      // the subsequent START_SUCCEEDED dispatch (fired by the shared
      // `startSignIn` callback) lands cleanly in awaitingActivation.
      // Without this reset, START_SUCCEEDED's `state.kind !== 'signedOut'`
      // gate would silently drop the action and the card would stay
      // stuck on the error/expired branch (issue #1241).
      // From `signedOut`/`awaitingActivation`/`signedIn`: still a no-op
      // — the click handler triggers an effect that calls
      // `startOpencodeDeviceFlowConsole`; the next dispatch is
      // START_SUCCEEDED or START_FAILED. Keeping the click handler
      // explicit (rather than mutating `busy: true` here) means the
      // card's only "loading" affordances live in the IPC-level effect.
      if (state.kind === 'error' || state.kind === 'signedInExpired') {
        return { kind: 'signedOut' };
      }
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

    case 'STATUS_FETCHED': {
      // On-mount restore from the persisted credential. Reached from
      // any state (the dance may have completed in a previous session)
      // so we don't gate on a specific `state.kind`. Three outcomes:
      //   - signed_in: false → no-op; stay in the current state so a
      //     `signedOut` mount doesn't flash a transient success.
      //   - signed_in: true, no active_workspace_id → no-op; the
      //     live probe needs the id to dispatch, so a credential
      //     missing the field is a "not signed in" state from the
      //     card's perspective.
      //   - signed_in: true, session_expired: true → signedInExpired.
      //   - signed_in: true, session_expired: false → signedIn.
      //
      // The active workspace is matched by id against the freshly
      // fetched workspace list; if the persisted id isn't in the
      // list (e.g. the user was removed from an org), fall back to
      // the first workspace so the card never lands in `signedIn`
      // with no active selection.
      const status = action.status;
      if (!status.signed_in) {
        return state;
      }
      if (status.workspaces.length === 0) {
        // No workspaces enumerable — treat as a not-signed-in state
        // from the UI's perspective. The user re-dances to refresh.
        return state;
      }
      const activeId = status.active_workspace_id;
      const active =
        (activeId && status.workspaces.find((w) => w.id === activeId)) ||
        status.workspaces[0];
      const expiresAtMs = status.access_token_expires_at_ms ?? 0;
      if (status.session_expired) {
        return {
          kind: 'signedInExpired',
          workspace: active,
          workspaces: status.workspaces,
          accessTokenExpiresAtMs: expiresAtMs,
        };
      }
      return {
        kind: 'signedIn',
        workspace: active,
        workspaces: status.workspaces,
        accessTokenExpiresAtMs: expiresAtMs,
      };
    }

    case 'WORKSPACE_CHOSEN_PENDING':
      // Optimistic: flip the picker to the new value AND mark the
      // switch as in-flight so the dropdown disables. The IPC fires
      // next (in the component effect); CONFIRMED / FAILED follows.
      // Allowed from both `signedIn` and `signedInExpired` — the
      // picker is also rendered in the expired branch (signing in
      // elsewhere may have refreshed a different account's token
      // before the user reopens Settings), so the same optimistic
      // flow has to work there.
      if (state.kind !== 'signedIn' && state.kind !== 'signedInExpired') return state;
      return {
        ...state,
        workspace: action.workspace,
        pendingWorkspaceSwitch: action.workspace,
      };

    case 'WORKSPACE_CHOSEN_CONFIRMED':
      // Clear the in-flight flag. The workspace is already set from
      // the optimistic PENDING transition. Strip the transient field
      // so the type stays narrow on subsequent renders (no stale
      // `pendingWorkspaceSwitch` on a user-initiated re-switch after
      // the prior one resolved).
      if (state.kind !== 'signedIn' && state.kind !== 'signedInExpired') return state;
      if (!state.pendingWorkspaceSwitch) return state;
      return {
        ...state,
        pendingWorkspaceSwitch: undefined,
      };

    case 'WORKSPACE_CHOSEN_FAILED':
      // Roll back the optimistic flip to the captured previous
      // workspace. The component additionally surfaces the error
      // message in a toast (see `OpenCodeAccountCard.tsx`).
      // Allowed from both `signedIn` and `signedInExpired` for the
      // same reason as PENDING.
      if (state.kind !== 'signedIn' && state.kind !== 'signedInExpired') return state;
      return {
        ...state,
        workspace: action.previousWorkspace,
      };

    case 'SIGNOUT_REQUESTED':
      // Issue #1241: the gate used to read `state.kind !== 'signedIn'`,
      // silently dropping the action from `signedInExpired`. The
      // component effect still fired `revokeOpencodeConsole()` — so the
      // backend credential WAS being revoked while the UI stayed in
      // `signedInExpired` forever, trapping the user with a dead "Sign
      // in again" button. The expired branch renders the same
      // two-step sign-out affordance as the fresh branch, so the
      // optimistic flip must accept either state.
      if (state.kind !== 'signedIn' && state.kind !== 'signedInExpired') return state;
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
