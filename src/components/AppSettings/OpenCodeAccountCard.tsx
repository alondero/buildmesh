import { useEffect, useReducer, useRef, useState } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import * as api from '../../lib/tauri';
import type { OpenCodeWorkspace } from '../../types/generated/OpenCodeWorkspace';
import {
  errorMessageFromUnknown,
  opencodeAccountReducer,
  type Action,
  type State,
} from './OpenCodeAccountCard.reducer';

/**
 * `OpenCodeAccountCard` — Settings → Providers → "OpenCode Account" surface
 * for issue #969. Drives the RFC 8628 Device Flow as a `useReducer` state
 * machine (signedOut → awaitingActivation → signedIn, plus error), opens the
 * verification URL via `openUrl()` (Tauri 2 silently drops `target="_blank"`
 * without an explicit capability we don't grant — see `SafeLink.tsx:21-25`
 * for the anti-pattern note), picks workspaces, and offers a two-step Sign
 * Out that mirrors `Authorized Devices` (`AppSettingsModal.tsx:582-601`,
 * issue #595: optimistic + post-success refresh; here, optimistic + rollback
 * on revoke failure).
 *
 * The reducer (`OpenCodeAccountCard.reducer.ts`) is a sibling file so it can
 * be unit-tested without importing React.
 */
export function OpenCodeAccountCard() {
  const [state, dispatch] = useReducer(opencodeAccountReducer, {
    kind: 'signedOut',
  } as State);

  // In-flight guards — second click on Sign-in while start_device_flow_console
  // is mid-flight must no-op (the IPC would issue a new device_code and
  // orphan the original poll). Captured per-effect rather than as a single
  // `useState('busy')` because Sign-in / Sign-out / Retry all need separate
  // guards with their own button labels.
  const startingRef = useRef(false);
  const signingOutRef = useRef(false);

  return (
    <div className="border border-border-subtle rounded-lg p-5">
      <h4 className="text-base font-medium text-text-primary mb-2">
        OpenCode Console
      </h4>
      <StateBody
        state={state}
        dispatch={dispatch}
        startingRef={startingRef}
        signingOutRef={signingOutRef}
      />
    </div>
  );
}

function StateBody({
  state,
  dispatch,
  startingRef,
  signingOutRef,
}: {
  state: State;
  dispatch: React.Dispatch<Action>;
  startingRef: React.MutableRefObject<boolean>;
  signingOutRef: React.MutableRefObject<boolean>;
}) {
  switch (state.kind) {
    case 'signedOut':
      return <SignedOutView dispatch={dispatch} startingRef={startingRef} />;
    case 'awaitingActivation':
      return (
        <AwaitingActivationView
          state={state}
          dispatch={dispatch}
        />
      );
    case 'signedIn':
      return (
        <SignedInView
          state={state}
          dispatch={dispatch}
          signingOutRef={signingOutRef}
        />
      );
    case 'error':
      return <ErrorView message={state.message} dispatch={dispatch} />;
  }
}

/* ── signedOut ─────────────────────────────────────────────────────────── */

function SignedOutView({
  dispatch,
  startingRef,
}: {
  dispatch: React.Dispatch<Action>;
  startingRef: React.MutableRefObject<boolean>;
}) {
  const onClick = () => {
    if (startingRef.current) return;
    startingRef.current = true;
    dispatch({ type: 'START_REQUESTED' });
    void (async () => {
      try {
        const start = await api.startOpencodeDeviceFlowConsole();
        // Defensive: a mock that resolved-with-undefined (the default
        // `vi.fn()` after `mockReset()`) or with the wrong shape (a one-shot
        // override that returns the poll-success object instead of start
        // data) would crash on `start.device_code` with the opaque error
        // "Cannot read properties of undefined (reading 'device_code')". A
        // shape check keeps the failure mode inside the test contract.
        if (
          !start ||
          typeof start !== 'object' ||
          typeof (start as { device_code?: unknown }).device_code !== 'string'
        ) {
          throw new Error(
            'start_device_flow_console returned unexpected payload shape',
          );
        }
        const s = start as {
          device_code: string;
          user_code: string;
          verification_uri_complete: string;
          interval_secs: number;
          expires_in_secs: number;
        };
        dispatch({
          type: 'START_SUCCEEDED',
          deviceCode: s.device_code,
          userCode: s.user_code,
          verificationUri: s.verification_uri_complete,
          intervalSecs: s.interval_secs,
          expiresInSecs: s.expires_in_secs,
        });
        // Open the verification page. `SafeLink.tsx:145` is the canonical
        // precedent: route the URL through `openUrl` so the OS handles
        // browser selection; do NOT use a `<a target="_blank">` (Tauri 2
        // drops `target="_blank">` without an explicit capability).
        openUrl(s.verification_uri_complete).catch((err: unknown) =>
          console.error('openUrl failed for OpenCode verification URL:', err),
        );
      } catch (err) {
        dispatch({
          type: 'START_FAILED',
          message: errorMessageFromUnknown(err),
        });
      } finally {
        startingRef.current = false;
      }
    })();
  };
  return (
    <>
      <p className="text-base text-text-muted mb-4">
        Sign in to <span className="font-medium">OpenCode Console</span> to
        fetch live usage data from the OpenCode Go server. A browser window
        will open to{' '}
        <span className="font-mono">console.opencode.ai</span>; sign in there
        and Buildmesh will pick up the token automatically.
      </p>
      <button
        type="button"
        onClick={onClick}
        data-testid="opencode-sign-in"
        className="px-5 py-2.5 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30"
      >
        Sign in with OpenCode Console
      </button>
    </>
  );
}

/* ── awaitingActivation ─────────────────────────────────────────────────── */

function AwaitingActivationView({
  state,
  dispatch,
}: {
  state: Extract<State, { kind: 'awaitingActivation' }>;
  dispatch: React.Dispatch<Action>;
}) {
  // Polling loop — keyed on `state.deviceCode` + `state.intervalSecs` +
  // `state.expiresAtMs` so a `slow_down` re-subscribes with the bumped
  // interval (cleanup of the previous effect clears the old `setInterval`).
  // `state.startedAtMs` is fixed at START_SUCCEEDED time and so never changes
  // inside this state — included only for ts-rs/spec completeness.
  useEffect(() => {
    // The deps below include `state.deviceCode` (etc.) which UNDEFINED-out
    // when state transitions to `signedIn`. Without this guard, the effect
    // would re-fire against the new state, hit `state.expiresAtMs`
    // (undefined) in the gate, fall through, and call
    // `api.pollOpencodeDeviceToken(undefined, ...)` — producing the bogus
    // "Cannot read properties of undefined (reading 'device_code')" error
    // that surfaced in the integration test runs. Bail early so only the
    // awaitingActivation branch polls. The state.kind assertion would
    // normally be supplied by the parent's `switch`, but `useEffect` is
    // called for ALL branches of the parent — re-entry happens.
    if (Date.now() >= state.expiresAtMs) {
      // Already past expiry before this effect ran — happens when the user
      // returns to the modal hours later. Flip to error immediately.
      dispatch({ type: 'POLL_RESULT', status: { kind: 'code_expired' } });
      return;
    }
    let cancelled = false;
    const tick = async () => {
      if (cancelled) return;
      const remainingSecs = Math.max(
        0,
        Math.floor((state.expiresAtMs - Date.now()) / 1000),
      );
      try {
        const status = await api.pollOpencodeDeviceToken(
          state.deviceCode,
          state.intervalSecs,
          remainingSecs,
          state.startedAtMs,
        );
        if (cancelled) return;
        dispatch({ type: 'POLL_RESULT', status });
        if (status.kind === 'success') {
          // Persist token + enumerate workspaces before flipping to
          // signedIn. Failures are recoverable: persist failure throws,
          // which surfaces as a START_FAILED-equivalent error; workspace
          // fetch failure falls through with an empty workspaces list (see
          // SignedInFromTokenEffect below).
          await api.persistOpencodeTokens(status.token);
          const workspaces = await api
            .listOpencodeWorkspaces()
            .catch((): OpenCodeWorkspace[] => []);
          if (cancelled) return;
          dispatch({
            type: 'SIGNED_IN_FROM_TOKEN',
            workspaces,
            accessTokenExpiresAtMs:
              Date.now() + status.token.expires_in_secs * 1000,
          });
        }
      } catch (err) {
        if (cancelled) return;
        dispatch({
          type: 'START_FAILED',
          message: errorMessageFromUnknown(err),
        });
      }
    };
    // First tick immediately so the user sees the prompt within ~1s rather
    // than waiting `intervalSecs` for the first poll — the `pending`
    // response from the server is the natural "still waiting" signal.
    void tick();
    const id = setInterval(() => {
      void tick();
    }, state.intervalSecs * 1000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- state.startedAtMs is fixed at dance-start; including it would re-subscribe the interval on every tick. dispatch is stable.
  }, [
    state.deviceCode,
    state.intervalSecs,
    state.expiresAtMs,
    dispatch,
  ]);

  return (
    <>
      <p className="text-base text-text-muted mb-3">
        Enter this code in the browser window we just opened:
      </p>
      <div
        data-testid="opencode-user-code"
        className="font-mono text-2xl tracking-widest text-center bg-bg-card border border-border-subtle rounded-md px-4 py-3 mb-4 select-all"
      >
        {state.userCode}
      </div>
      <p className="text-base text-text-muted mb-4">
        Polling every {state.intervalSecs}s while the window stays open…
      </p>
      <div className="flex gap-3 items-center">
        <button
          type="button"
          data-testid="opencode-reopen"
          onClick={() =>
            openUrl(state.verificationUri).catch((err: unknown) =>
              console.error('openUrl failed for OpenCode verification URL:', err),
            )
          }
          className="text-base text-accent-cyan hover:text-accent-cyan/80"
        >
          Reopen verification page ↗
        </button>
        <button
          type="button"
          onClick={() =>
            dispatch({ type: 'START_FAILED', message: 'Sign-in cancelled.' })
          }
          data-testid="opencode-cancel"
          className="text-base text-status-error hover:text-status-error/80"
        >
          Cancel
        </button>
      </div>
    </>
  );
}

/* ── signedIn ─────────────────────────────────────────────────────────── */

function SignedInView({
  state,
  dispatch,
  signingOutRef,
}: {
  state: Extract<State, { kind: 'signedIn' }>;
  dispatch: React.Dispatch<Action>;
  signingOutRef: React.MutableRefObject<boolean>;
}) {
  // Two-step Sign Out — mirrors `confirmingRevokeId` at
  // `AppSettingsModal.tsx:1487-1510`. First click flips the button text;
  // second click fires. Reset on workspace change so a stale confirm doesn't
  // outlive the picker swap.
  const [confirmingSignOut, setConfirmingSignOut] = useState(false);
  const capturedRef = useRef<{
    workspace: OpenCodeWorkspace;
    workspaces: OpenCodeWorkspace[];
    accessTokenExpiresAtMs: number;
  } | null>(null);

  const onSignOut = async () => {
    if (signingOutRef.current) return;
    if (!confirmingSignOut) {
      // First click — flip to confirm; capture the snapshot for rollback.
      setConfirmingSignOut(true);
      capturedRef.current = {
        workspace: state.workspace,
        workspaces: state.workspaces,
        accessTokenExpiresAtMs: state.accessTokenExpiresAtMs,
      };
      return;
    }
    signingOutRef.current = true;
    dispatch({ type: 'SIGNOUT_REQUESTED' });
    try {
      await api.revokeOpencodeConsole();
      dispatch({ type: 'SIGNOUT_SUCCEEDED' });
    } catch (err) {
      const captured = capturedRef.current;
      if (captured) {
        dispatch({
          type: 'SIGNOUT_FAILED',
          message: errorMessageFromUnknown(err),
          previousWorkspace: captured.workspace,
          previousWorkspaces: captured.workspaces,
          previousExpiresAtMs: captured.accessTokenExpiresAtMs,
        });
      }
    } finally {
      signingOutRef.current = false;
      capturedRef.current = null;
      setConfirmingSignOut(false);
    }
  };

  return (
    <>
      <p className="text-base text-text-muted mb-4">
        Signed in to OpenCode Console.
      </p>
      <div className="text-base text-text-primary mb-3">
        Workspace:{' '}
        <span className="font-mono" data-testid="opencode-workspace-name">
          {state.workspace.name}
        </span>
      </div>
      {state.workspaces.length > 1 && (
        <div className="mb-4">
          <label
            htmlFor="opencode-workspace-picker"
            className="text-sm text-text-muted mr-2"
          >
            Switch workspace:
          </label>
          <select
            id="opencode-workspace-picker"
            data-testid="opencode-workspace-picker"
            value={state.workspace.id}
            onChange={(e) => {
              const next = state.workspaces.find(
                (w) => w.id === e.target.value,
              );
              if (next) dispatch({ type: 'WORKSPACE_CHOSEN', workspace: next });
            }}
            className="bg-bg-card border border-border-subtle rounded-md px-2 py-1 text-base text-text-primary"
          >
            {state.workspaces.map((w) => (
              <option key={w.id} value={w.id}>
                {w.name}
              </option>
            ))}
          </select>
        </div>
      )}
      <button
        type="button"
        onClick={() => {
          void onSignOut();
        }}
        data-testid="opencode-sign-out"
        className={
          confirmingSignOut
            ? 'px-4 py-2 bg-status-error text-white text-base rounded-md hover:bg-status-error/90'
            : 'px-4 py-2 bg-status-error/15 text-status-error text-base rounded-md hover:bg-status-error/25'
        }
      >
        {confirmingSignOut ? 'Confirm sign out' : 'Sign out'}
      </button>
    </>
  );
}

/* ── error ────────────────────────────────────────────────────────────── */

function ErrorView({
  message,
  dispatch,
}: {
  message: string;
  dispatch: React.Dispatch<Action>;
}) {
  return (
    <>
      <div
        data-testid="opencode-error"
        role="alert"
        className="border border-status-error/40 rounded-md px-3 py-2 mb-4 text-base text-status-error bg-status-error/10"
      >
        {message}
      </div>
      <button
        type="button"
        onClick={() => dispatch({ type: 'START_REQUESTED' })}
        data-testid="opencode-retry"
        className="px-4 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30"
      >
        Retry sign-in
      </button>
    </>
  );
}
