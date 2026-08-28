import { describe, it, expect } from 'vitest';
import {
  errorMessageFromUnknown,
  opencodeAccountReducer,
  type State,
} from '../../src/components/AppSettings/OpenCodeAccountCard.reducer';
import type { OpenCodeWorkspace } from '../../src/types/generated/OpenCodeWorkspace';

/**
 * Reducer unit tests for the OpenCode OAuth Device Flow state machine
 * (issue #969). Pure-function coverage — every transition + every error
 * variant of `OpenCodeDeviceCodeStatus`. The reducer ships in a sibling file
 * (`OpenCodeAccountCard.reducer.ts`) so it can be tested without importing
 * React or the component shell.
 *
 * The component test file (`opencode-account-card.test.tsx`) wires the
 * reducer to Tauri `invoke` mocks; this file is the regression net for the
 * state-machine transitions themselves.
 */

const ws1: OpenCodeWorkspace = { id: 'wrk_a', name: 'Acme' };
const ws2: OpenCodeWorkspace = { id: 'wrk_b', name: 'Beta' };
const ws3: OpenCodeWorkspace = { id: 'wrk_c', name: 'Gamma' };

const signedOut: State = { kind: 'signedOut' };

// Issue #1101: helpers below are called twice in the same assertion (input
// state + expected value). A `Date.now()` default makes the two reads land
// on different milliseconds and flakes `toEqual` (regularly on Windows,
// where timer granularity is coarse). Pin the clock to a fixed instant so
// both calls produce identical values by construction.
const FIXED_NOW_MS = 1_700_000_000_000;

const awaiting = (over: Partial<Extract<State, { kind: 'awaitingActivation' }>> = {}): State => ({
  kind: 'awaitingActivation',
  deviceCode: 'dc_test',
  userCode: 'WXYZ-1234',
  verificationUri: 'https://console.opencode.ai/auth/device?code=WXYZ-1234',
  intervalSecs: 5,
  expiresAtMs: FIXED_NOW_MS + 600_000,
  originalExpiresInSecs: 600,
  startedAtMs: FIXED_NOW_MS - 1000,
  ...over,
});

const signedIn = (over: Partial<Extract<State, { kind: 'signedIn' }>> = {}): State => ({
  kind: 'signedIn',
  workspace: ws1,
  workspaces: [ws1, ws2, ws3],
  accessTokenExpiresAtMs: FIXED_NOW_MS + 600_000,
  ...over,
});

describe('opencodeAccountReducer (issue #969)', () => {
  it('START_REQUESTED is a no-op from signedOut', () => {
    expect(opencodeAccountReducer(signedOut, { type: 'START_REQUESTED' })).toEqual(signedOut);
  });

  it('START_SUCCEEDED from signedOut populates awaitingActivation with now-relative timestamps', () => {
    const before = Date.now();
    const next = opencodeAccountReducer(signedOut, {
      type: 'START_SUCCEEDED',
      deviceCode: 'dc_x',
      userCode: 'ABCD-1234',
      verificationUri: 'https://console.opencode.ai/auth/device?code=ABCD-1234',
      intervalSecs: 7,
      originalExpiresInSecs: 600,
    });
    expect(next.kind).toBe('awaitingActivation');
    if (next.kind !== 'awaitingActivation') return;
    expect(next.deviceCode).toBe('dc_x');
    expect(next.userCode).toBe('ABCD-1234');
    expect(next.verificationUri).toBe('https://console.opencode.ai/auth/device?code=ABCD-1234');
    expect(next.intervalSecs).toBe(7);
    // startedAtMs is `Date.now()` at dispatch time; allow up to 50ms drift.
    expect(next.startedAtMs).toBeGreaterThanOrEqual(before);
    expect(next.startedAtMs).toBeLessThanOrEqual(Date.now());
    expect(next.expiresAtMs).toBeGreaterThanOrEqual(before + 600 * 1000 - 50);
    expect(next.expiresAtMs).toBeLessThanOrEqual(Date.now() + 600 * 1000 + 50);
    // Issue #1010: the immutable ORIGINAL window length is stored verbatim
    // on the state — the IPC sends this each tick (not a per-tick countdown)
    // so the Rust expiry gate stays monotonic across the full window.
    expect(next.originalExpiresInSecs).toBe(600);
  });

  it('START_SUCCEEDED is dropped if the reducer was already off the signedOut branch', () => {
    // Edge: user clicks Sign-in twice before the first IPC resolves; the
    // second click should NOT clobber a dance already in flight. Reducer is
    // idempotent on this action once state has left signedOut.
    const next = opencodeAccountReducer(awaiting(), {
      type: 'START_SUCCEEDED',
      deviceCode: 'dc_late',
      userCode: 'LATE-9999',
      verificationUri: 'https://late',
      intervalSecs: 9,
      originalExpiresInSecs: 600,
    });
    expect(next).toEqual(awaiting());
  });

  it('POLL_RESULT.pending keeps awaitingActivation with unchanged interval', () => {
    const before = awaiting({ intervalSecs: 5 });
    const next = opencodeAccountReducer(before, {
      type: 'POLL_RESULT',
      status: { kind: 'pending' },
    });
    expect(next).toBe(before); // pure same-reference return; structural equal too.
    expect(next).toEqual(before);
  });

  it('POLL_RESULT.slow_down bumps interval but stays in awaitingActivation', () => {
    const before = awaiting({ intervalSecs: 5 });
    const next = opencodeAccountReducer(before, {
      type: 'POLL_RESULT',
      status: { kind: 'slow_down', new_interval_secs: 10 },
    });
    expect(next.kind).toBe('awaitingActivation');
    if (next.kind !== 'awaitingActivation') return;
    expect(next.intervalSecs).toBe(10);
    expect(next.deviceCode).toBe(before.deviceCode);
    expect(next.userCode).toBe(before.userCode);
  });

  it('POLL_RESULT.success stays awaitingActivation — the SIGNED_IN_FROM_TOKEN effect is what flips to signedIn', () => {
    // Issue #969: the dance's terminal "token acquired" state must wait for
    // list_opencode_workspaces to resolve, so a race in either direction
    // (workspaces resolves first, token persists after) can't render signedIn
    // without a workspace to bind to.
    const before = awaiting();
    const next = opencodeAccountReducer(before, {
      type: 'POLL_RESULT',
      status: {
        kind: 'success',
        token: {
          access_token: 'at_x',
          refresh_token: 'rt_x',
          expires_in_secs: 600,
        },
      },
    });
    expect(next).toEqual(before);
  });

  it('POLL_RESULT.code_expired transitions awaitingActivation → error', () => {
    const next = opencodeAccountReducer(awaiting(), {
      type: 'POLL_RESULT',
      status: { kind: 'code_expired' },
    });
    expect(next).toEqual({
      kind: 'error',
      message: 'OpenCode sign-in timed out. Please start the dance again.',
    });
  });

  it('POLL_RESULT.access_denied transitions awaitingActivation → error', () => {
    const next = opencodeAccountReducer(awaiting(), {
      type: 'POLL_RESULT',
      status: { kind: 'access_denied' },
    });
    expect(next).toEqual({
      kind: 'error',
      message: 'OpenCode sign-in was denied at the consent prompt.',
    });
  });

  it('POLL_RESULT.error transitions awaitingActivation → error with the underlying message', () => {
    const next = opencodeAccountReducer(awaiting(), {
      type: 'POLL_RESULT',
      status: { kind: 'error', message: 'DNS resolution failed for console.opencode.ai' },
    });
    expect(next).toEqual({
      kind: 'error',
      message: 'OpenCode OAuth failed: DNS resolution failed for console.opencode.ai',
    });
  });

  it('SIGNED_IN_FROM_TOKEN transitions awaitingActivation → signedIn with first workspace', () => {
    const next = opencodeAccountReducer(awaiting(), {
      type: 'SIGNED_IN_FROM_TOKEN',
      workspaces: [ws1, ws2, ws3],
      accessTokenExpiresAtMs: 9_999_999_999,
    });
    expect(next).toEqual({
      kind: 'signedIn',
      workspace: ws1,
      workspaces: [ws1, ws2, ws3],
      accessTokenExpiresAtMs: 9_999_999_999,
    });
  });

  it('SIGNED_IN_FROM_TOKEN with empty workspaces surfaces an error instead of a vacuous signedIn', () => {
    const next = opencodeAccountReducer(awaiting(), {
      type: 'SIGNED_IN_FROM_TOKEN',
      workspaces: [],
      accessTokenExpiresAtMs: 9_999_999_999,
    });
    expect(next.kind).toBe('error');
    if (next.kind !== 'error') return;
    expect(next.message).toMatch(/No workspaces/i);
  });

  it('WORKSPACE_CHOSEN_PENDING optimistically flips the workspace AND marks the switch in-flight', () => {
    const before = signedIn({ workspace: ws1 });
    const next = opencodeAccountReducer(before, {
      type: 'WORKSPACE_CHOSEN_PENDING',
      workspace: ws2,
    });
    expect(next.kind).toBe('signedIn');
    if (next.kind !== 'signedIn') return;
    expect(next.workspace).toEqual(ws2);
    expect(next.pendingWorkspaceSwitch).toEqual(ws2);
  });

  it('WORKSPACE_CHOSEN_PENDING is allowed from signedInExpired (the picker renders there too)', () => {
    // Spec fix: the expired branch renders the picker so a user can
    // switch to an account whose token they have already re-issued
    // elsewhere. The reducer arm must accept this state — pre-fix
    // it gated on `kind === 'signedIn'` and silently dropped the
    // action, leaving the user with a working-looking picker that
    // did nothing on click.
    const before: State = {
      kind: 'signedInExpired',
      workspace: ws1,
      workspaces: [ws1, ws2],
      accessTokenExpiresAtMs: 0,
    };
    const next = opencodeAccountReducer(before, {
      type: 'WORKSPACE_CHOSEN_PENDING',
      workspace: ws2,
    });
    expect(next.kind).toBe('signedInExpired');
    if (next.kind !== 'signedInExpired') return;
    expect(next.workspace).toEqual(ws2);
    expect(next.pendingWorkspaceSwitch).toEqual(ws2);
  });

  it('WORKSPACE_CHOSEN_CONFIRMED clears the in-flight flag, preserves the new workspace', () => {
    const before = signedIn({ workspace: ws2 });
    before.pendingWorkspaceSwitch = ws2;
    const next = opencodeAccountReducer(before, {
      type: 'WORKSPACE_CHOSEN_CONFIRMED',
    });
    expect(next.kind).toBe('signedIn');
    if (next.kind !== 'signedIn') return;
    expect(next.workspace).toEqual(ws2);
    expect(next.pendingWorkspaceSwitch).toBeUndefined();
  });

  it('WORKSPACE_CHOSEN_FAILED rolls back to the captured previousWorkspace', () => {
    const before = signedIn({ workspace: ws2 });
    before.pendingWorkspaceSwitch = ws2;
    const next = opencodeAccountReducer(before, {
      type: 'WORKSPACE_CHOSEN_FAILED',
      previousWorkspace: ws1,
      message: 'IPC failed',
    });
    expect(next.kind).toBe('signedIn');
    if (next.kind !== 'signedIn') return;
    expect(next.workspace).toEqual(ws1);
  });

  it('STATUS_FETCHED with signed_in=false is a no-op from any state', () => {
    expect(
      opencodeAccountReducer(signedIn(), {
        type: 'STATUS_FETCHED',
        status: {
          signed_in: false,
          workspaces: [],
          active_workspace_id: null,
          access_token_expires_at_ms: null,
          session_expired: false,
        },
      }),
    ).toEqual(signedIn());
    expect(
      opencodeAccountReducer({ kind: 'signedOut' }, {
        type: 'STATUS_FETCHED',
        status: {
          signed_in: false,
          workspaces: [],
          active_workspace_id: null,
          access_token_expires_at_ms: null,
          session_expired: false,
        },
      }),
    ).toEqual({ kind: 'signedOut' });
  });

  it('STATUS_FETCHED with signed_in=true and session_expired=false transitions signedOut → signedIn matching the active workspace', () => {
    const next = opencodeAccountReducer(
      { kind: 'signedOut' },
      {
        type: 'STATUS_FETCHED',
        status: {
          signed_in: true,
          workspaces: [ws1, ws2],
          active_workspace_id: 'wrk_b',
          access_token_expires_at_ms: 1_700_000_000_000,
          session_expired: false,
        },
      },
    );
    expect(next).toEqual({
      kind: 'signedIn',
      workspace: ws2,
      workspaces: [ws1, ws2],
      accessTokenExpiresAtMs: 1_700_000_000_000,
    });
  });

  it('STATUS_FETCHED with session_expired=true transitions to signedInExpired', () => {
    const next = opencodeAccountReducer(
      { kind: 'signedOut' },
      {
        type: 'STATUS_FETCHED',
        status: {
          signed_in: true,
          workspaces: [ws1, ws2],
          active_workspace_id: 'wrk_a',
          access_token_expires_at_ms: 1_500_000_000_000, // long past
          session_expired: true,
        },
      },
    );
    expect(next.kind).toBe('signedInExpired');
    if (next.kind !== 'signedInExpired') return;
    expect(next.workspace).toEqual(ws1);
  });

  it('STATUS_FETCHED falls back to the first workspace when active_workspace_id is not in the list', () => {
    // Defensive: a user removed from an org (their previously-active
    // org id no longer in the freshly-enumerated picker) shouldn't
    // land in `signedIn` with no active selection. The reducer
    // falls back to slot 0 (their personal identity).
    const next = opencodeAccountReducer(
      { kind: 'signedOut' },
      {
        type: 'STATUS_FETCHED',
        status: {
          signed_in: true,
          workspaces: [ws1, ws2],
          active_workspace_id: 'wrk_orphaned',
          access_token_expires_at_ms: 1_700_000_000_000,
          session_expired: false,
        },
      },
    );
    expect(next.kind).toBe('signedIn');
    if (next.kind !== 'signedIn') return;
    expect(next.workspace).toEqual(ws1);
  });

  it('SIGNOUT_REQUESTED optimistically flips signedIn → signedOut', () => {
    const next = opencodeAccountReducer(signedIn(), { type: 'SIGNOUT_REQUESTED' });
    expect(next).toEqual({ kind: 'signedOut' });
  });

  it('SIGNOUT_REQUESTED from signedInExpired also transitions to signedOut', () => {
    // Issue #1241: the gate used to read `state.kind !== 'signedIn'`,
    // which silently dropped the action from `signedInExpired`. The
    // card's `SignedInExpiredView` still calls
    // `api.revokeOpencodeConsole()` — so the credential WAS being
    // revoked while the UI stayed in `signedInExpired` forever,
    // trapping the user. The reducer must accept either state so the
    // optimistic flip matches the file's own docstring (lines 33-35).
    const before: State = {
      kind: 'signedInExpired',
      workspace: ws1,
      workspaces: [ws1, ws2],
      accessTokenExpiresAtMs: 1_500_000_000_000,
    };
    const next = opencodeAccountReducer(before, { type: 'SIGNOUT_REQUESTED' });
    expect(next).toEqual({ kind: 'signedOut' });
  });

  it('SIGNOUT_FAILED rolls back signedOut → signedIn with the captured snapshot', () => {
    const capturedWorkspace = ws2;
    const capturedWorkspaces = [ws2, ws3];
    const capturedExpires = 7_777_777;
    // SIGNOUT_REQUESTED first to simulate the optimistic flip.
    const afterRequest = opencodeAccountReducer(signedIn(), { type: 'SIGNOUT_REQUESTED' });
    expect(afterRequest.kind).toBe('signedOut');
    const afterFail = opencodeAccountReducer(afterRequest, {
      type: 'SIGNOUT_FAILED',
      message: 'Windows credential store inaccessible',
      previousWorkspace: capturedWorkspace,
      previousWorkspaces: capturedWorkspaces,
      previousExpiresAtMs: capturedExpires,
    });
    expect(afterFail).toEqual({
      kind: 'signedIn',
      workspace: capturedWorkspace,
      workspaces: capturedWorkspaces,
      accessTokenExpiresAtMs: capturedExpires,
    });
  });

  it('SIGNOUT_SUCCEEDED stays in signedOut', () => {
    const next = opencodeAccountReducer(signedOut, { type: 'SIGNOUT_SUCCEEDED' });
    expect(next).toEqual(signedOut);
  });

  it('START_FAILED from signedOut transitions to error', () => {
    const next = opencodeAccountReducer(signedOut, {
      type: 'START_FAILED',
      message: 'Network unreachable',
    });
    expect(next).toEqual({ kind: 'error', message: 'Network unreachable' });
  });

  it('START_FAILED from awaitingActivation also transitions to error', () => {
    // The Cancel button dispatches START_FAILED rather than a dedicated
    // CANCEL action (start_failed is the canonical "the dance is dead"
    // signal and the reducer needs to handle it from either branch).
    const next = opencodeAccountReducer(awaiting(), {
      type: 'START_FAILED',
      message: 'Sign-in cancelled.',
    });
    expect(next).toEqual({ kind: 'error', message: 'Sign-in cancelled.' });
  });
});

describe('errorMessageFromUnknown', () => {
  it('extracts the message from an Error instance', () => {
    expect(errorMessageFromUnknown(new Error('boom'))).toBe('boom');
  });

  it('returns string inputs verbatim', () => {
    expect(errorMessageFromUnknown('the string itself')).toBe('the string itself');
  });

  it('JSON-stringifies plain objects', () => {
    const out = errorMessageFromUnknown({ code: 42 });
    expect(out).toBe('{"code":42}');
  });

  it('falls back to a sentinel for non-serializable throws', () => {
    const circular: Record<string, unknown> = {};
    circular.self = circular;
    expect(errorMessageFromUnknown(circular)).toBe('Unknown error');
  });
});
