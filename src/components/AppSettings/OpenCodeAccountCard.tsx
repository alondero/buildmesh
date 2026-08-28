import { useEffect, useReducer, useRef, useState } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import * as api from '../../lib/tauri';
import type { OpenCodeWorkspace } from '../../types/generated/OpenCodeWorkspace';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { addToast } from '../../stores/toastStore';
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

  // Issue #1241: the start-the-dance routine used to live inside
  // SignedOutView.onClick only — the Retry button on the error branch
  // and the "Sign in again" button on the signedInExpired branch both
  // dispatched START_REQUESTED, which the reducer treats as a no-op by
  // design. Now that all three recovery entry points must run the IPC,
  // the routine lives at the parent (where `startingRef` lives) and is
  // passed down to every view that needs to start a fresh dance.
  const startSignIn = () => {
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
          // The IPC carries the ORIGINAL window length verbatim — not a
          // per-tick countdown. Pre-fix (#1010) the component computed
          // `(expiresAtMs - Date.now()) / 1000` per tick and sent that as
          // `expiresInSecs`, which made the Rust gate fire at the halfway
          // point. See `OpenCodeAccountCard.reducer.ts` for the rename.
          originalExpiresInSecs: s.expires_in_secs,
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

  // Mount-time restore: if a credential is already sitting in
  // Windows Credential Manager (the user previously signed in
  // successfully and re-opens Settings), transition to `signedIn`
  // without re-running the dance. The IPC
  // `get_opencode_console_status` bundles the workspace list with the
  // active workspace id and the access-token expiry epoch so the
  // `STATUS_FETCHED` reducer arm can pick the right sub-state
  // (`signedIn` vs `signedInExpired`).
  //
  // `signal.aborted` gates the setState so a fast modal close during
  // the IPC roundtrip doesn't dispatch into a stale component
  // instance (mirrors the `UsageTab` mount-time fetch idiom,
  // `src/components/Probe/UsageTab.tsx:185-187`).
  useAsyncEffect((signal) => {
    void api
      .getOpencodeConsoleStatus()
      .then((status) => {
        if (signal.aborted) return;
        // Defensive: a unit test that pre-dates the
        // `get_opencode_console_status` IPC may leave its mock
        // returning `undefined`. A real backend failure
        // (rare) would also resolve to `undefined` from
        // `_invoke`'s error path. Either way, treat as "not
        // signed in" — the dance is still reachable from
        // `signedOut`, and the user can re-dance to recover.
        if (!status || typeof status !== 'object') return;
        dispatch({ type: 'STATUS_FETCHED', status });
      })
      .catch(() => {
        // Best-effort: a credential-store read failure should not
        // crash the Settings modal. The user sees the default
        // `signedOut` UI and can re-dance to recover.
      });
  }, []);

  return (
    <div className="border border-border-subtle rounded-lg p-5">
      <h4 className="text-base font-medium text-text-primary mb-2">
        OpenCode Console
      </h4>
      <StateBody
        state={state}
        dispatch={dispatch}
        startSignIn={startSignIn}
        signingOutRef={signingOutRef}
      />
    </div>
  );
}

function StateBody({
  state,
  dispatch,
  startSignIn,
  signingOutRef,
}: {
  state: State;
  dispatch: React.Dispatch<Action>;
  startSignIn: () => void;
  signingOutRef: React.MutableRefObject<boolean>;
}) {
  switch (state.kind) {
    case 'signedOut':
      return <SignedOutView startSignIn={startSignIn} />;
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
    case 'signedInExpired':
      return (
        <SignedInExpiredView
          state={state}
          dispatch={dispatch}
          startSignIn={startSignIn}
          signingOutRef={signingOutRef}
        />
      );
    case 'error':
      return <ErrorView message={state.message} startSignIn={startSignIn} />;
  }
}

/* ── signedOut ─────────────────────────────────────────────────────────── */

function SignedOutView({
  startSignIn,
}: {
  startSignIn: () => void;
}) {
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
        onClick={startSignIn}
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
  // interval (cleanup of the previous effect clears the old
  // `setInterval`). `state.originalExpiresInSecs` + `state.startedAtMs`
  // are both fixed at START_SUCCEEDED time and never change inside this
  // state — kept off the dep list (the eslint-disable below) so we
  // don't re-subscribe the interval on every render. `expiresAtMs` IS
  // in the deps because it's read once on mount for the pre-flight
  // gate (`Date.now() >= state.expiresAtMs`).
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
      try {
        // Ship the immutable ORIGINAL window length each tick — NOT a
        // per-tick countdown. Pre-fix (#1010) the third arg was computed
        // here as `(state.expiresAtMs - Date.now()) / 1000`, which made
        // the Rust gate `now_ms - started_at_ms >= remaining*1000` fire
        // when `elapsed == remaining` (the halfway point of the window).
        // Storing the value at dance-start time and sending it verbatim
        // each tick keeps the gate monotonic across the full window.
        const status = await api.pollOpencodeDeviceToken(
          state.deviceCode,
          state.intervalSecs,
          state.originalExpiresInSecs,
          state.startedAtMs,
        );
        if (cancelled) return;
        dispatch({ type: 'POLL_RESULT', status });
        if (status.kind === 'success') {
          // Enumerate workspaces FIRST so we can thread the OAuth-scoped
          // workspace_id into the persisted token blob. The live server's
          // token response (verified 2026-07-23) does NOT carry
          // workspace_id — the live `_server billing.get` probe at
          // services::usage::opencode_live_request_parts requires it, so
          // we must source it from GET /api/user (the first entry in the
          // list_opencode_workspaces result) before persisting.
          //
          // Pass the freshly-polled access_token explicitly: on a
          // first-time sign-in the credential blob has NOT been written
          // yet, so the IPC's read-from-Credential-Manager fallback would
          // see nothing and return `[]`. The token-bearing path avoids
          // that hole and keeps the persisted workspace_id non-empty.
          const workspaces = await api
            .listOpencodeWorkspaces(status.token.access_token)
            .catch((): OpenCodeWorkspace[] => []);
          if (cancelled) return;
          const firstWorkspaceId = workspaces[0]?.id;
          await api.persistOpencodeTokens(
            status.token,
            firstWorkspaceId,
            undefined,
          );
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
    // eslint-disable-next-line react-hooks/exhaustive-deps -- state.startedAtMs is fixed at dance-start; including it would re-subscribe the interval on every tick. dispatch is stable. state.originalExpiresInSecs is also fixed at dance-start so we keep it off the dep list for the same reason — the effect should only re-subscribe when intervalSecs changes (slow_down bump), not on every render.
  }, [
    state.deviceCode,
    state.intervalSecs,
    state.expiresAtMs,
    dispatch,
  ]);

  return (
    <>
      <p className="text-base text-text-muted mb-3">
        Enter this code in the browser window we just opened. If the window
        didn&apos;t open, copy the link below into your browser:
      </p>
      <div
        data-testid="opencode-user-code"
        className="font-mono text-2xl tracking-widest text-center bg-bg-card border border-border-subtle rounded-md px-4 py-3 mb-3 select-all"
      >
        {state.userCode}
      </div>
      {/* Fallback so the user can copy/paste the URL if `openUrl()` fails
          for any reason — capability drift, OS default-browser mis-config,
          or a Tauri regression. Mirrors the user-code block above. */}
      <div
        data-testid="opencode-verification-url"
        className="font-mono text-sm break-all bg-bg-card border border-border-subtle rounded-md px-3 py-2 mb-4 select-all"
      >
        {state.verificationUri}
      </div>
      <p className="text-base text-text-muted mb-4">
        Polling every {state.intervalSecs}s while the window stays open…
      </p>
      <div className="flex gap-3 items-center">
        {/* Dual-route link matching the `SafeLink` pattern: the `<a href>`
            keeps right-click → "Open in browser", ⌘-click, and screen-reader
            fallbacks alive even if `openUrl()` fails. `target="_blank"` is
            a no-op in Tauri 2 without `core:webview:allow-create-webview-window`
            (we don't grant it), so the onClick calls `preventDefault` +
            `stopPropagation` and routes through `openUrl()` instead. */}
        <a
          href={state.verificationUri}
          target="_blank"
          rel="noopener noreferrer"
          data-testid="opencode-verification-link"
          className="text-base text-accent-cyan hover:text-accent-cyan/80"
          onClick={(e) => {
            e.preventDefault();
            e.stopPropagation();
            openUrl(state.verificationUri).catch((err: unknown) =>
              console.error('openUrl failed for OpenCode verification URL:', err),
            );
          }}
        >
          Reopen verification page ↗
        </a>
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
        Account:{' '}
        <span className="font-mono" data-testid="opencode-account-name">
          {formatAccountLabel(state.workspace, state.workspaces)}
        </span>
      </div>
      {state.workspaces.length > 1 && (
        <p className="text-xs text-text-muted mb-4">
          Choose which account's live usage to fetch. You can switch at any time.
        </p>
      )}
      {state.workspaces.length > 1 && (
        <div className="mb-4">
          <label
            htmlFor="opencode-account-picker"
            className="text-sm text-text-muted mr-2"
          >
            Switch account:
          </label>
          <select
            id="opencode-account-picker"
            data-testid="opencode-account-picker"
            value={state.workspace.id}
            disabled={state.pendingWorkspaceSwitch != null}
            onChange={(e) => {
              const next = state.workspaces.find(
                (w) => w.id === e.target.value,
              );
              if (!next || next.id === state.workspace.id) return;
              // Optimistic flip + IPC. The reducer transitions
              // PENDING → CONFIRMED on resolve, PENDING → FAILED on
              // reject (with rollback to the captured previous
              // workspace). Captures the previous workspace for the
              // reducer's rollback path.
              const previous = state.workspace;
              dispatch({ type: 'WORKSPACE_CHOSEN_PENDING', workspace: next });
              api
                .setOpencodeConsoleWorkspace(next.id)
                .then(() => {
                  dispatch({ type: 'WORKSPACE_CHOSEN_CONFIRMED' });
                })
                .catch((err: unknown) => {
                  // Surface the error in a toast AND roll the
                  // reducer back — the picker flip is purely
                  // optimistic and would leave the user with no
                  // signal that the switch failed. The toast
                  // survives a Settings modal close, unlike a
                  // banner.
                  const message = errorMessageFromUnknown(err);
                  addToast('opencode', `Switch account failed: ${message}`);
                  dispatch({
                    type: 'WORKSPACE_CHOSEN_FAILED',
                    previousWorkspace: previous,
                    message,
                  });
                });
            }}
            className="bg-bg-card border border-border-subtle rounded-md px-2 py-1 text-base text-text-primary"
          >
            {state.workspaces.map((w) => (
              <option key={w.id} value={w.id}>
                {formatAccountLabel(w, state.workspaces)}
              </option>
            ))}
          </select>
          {state.pendingWorkspaceSwitch && (
            <span
              className="ml-3 text-xs text-text-muted"
              data-testid="opencode-account-switch-pending"
            >
              Switching…
            </span>
          )}
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

/* ── signedInExpired ─────────────────────────────────────────────────── */

function SignedInExpiredView({
  state,
  dispatch,
  startSignIn,
  signingOutRef,
}: {
  state: Extract<State, { kind: 'signedInExpired' }>;
  dispatch: React.Dispatch<Action>;
  startSignIn: () => void;
  signingOutRef: React.MutableRefObject<boolean>;
}) {
  // Same affordances as SignedInView (switch account still works
  // even with an expired session — the live probe will surface the
  // 401 and reactive refresh-on-401 will self-heal), plus a
  // yellow "Session expired" banner and a "Sign in again" button
  // that kicks off a fresh dance.
  const [confirmingSignOut, setConfirmingSignOut] = useState(false);
  const capturedRef = useRef<{
    workspace: OpenCodeWorkspace;
    workspaces: OpenCodeWorkspace[];
    accessTokenExpiresAtMs: number;
  } | null>(null);

  const onSignOut = async () => {
    if (signingOutRef.current) return;
    if (!confirmingSignOut) {
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
      <div
        data-testid="opencode-session-expired"
        role="status"
        className="border border-status-warning/40 rounded-md px-3 py-2 mb-4 text-base text-status-warning bg-status-warning/10"
      >
        Session expired — re-authenticate to refresh usage.
      </div>
      <p className="text-base text-text-muted mb-4">
        Last account:{' '}
        <span className="font-mono" data-testid="opencode-account-name">
          {formatAccountLabel(state.workspace, state.workspaces)}
        </span>
      </p>
      {state.workspaces.length > 1 && (
        <div className="mb-4">
          <label
            htmlFor="opencode-account-picker"
            className="text-sm text-text-muted mr-2"
          >
            Switch account:
          </label>
          <select
            id="opencode-account-picker"
            data-testid="opencode-account-picker"
            value={state.workspace.id}
            disabled={state.pendingWorkspaceSwitch != null}
            onChange={(e) => {
              const next = state.workspaces.find(
                (w) => w.id === e.target.value,
              );
              if (!next || next.id === state.workspace.id) return;
              const previous = state.workspace;
              dispatch({ type: 'WORKSPACE_CHOSEN_PENDING', workspace: next });
              api
                .setOpencodeConsoleWorkspace(next.id)
                .then(() => {
                  dispatch({ type: 'WORKSPACE_CHOSEN_CONFIRMED' });
                })
                .catch((err: unknown) => {
                  // Surface the error in a toast AND roll the
                  // reducer back — the picker flip is purely
                  // optimistic and would leave the user with no
                  // signal that the switch failed. The toast
                  // survives a Settings modal close, unlike a
                  // banner.
                  const message = errorMessageFromUnknown(err);
                  addToast('opencode', `Switch account failed: ${message}`);
                  dispatch({
                    type: 'WORKSPACE_CHOSEN_FAILED',
                    previousWorkspace: previous,
                    message,
                  });
                });
            }}
            className="bg-bg-card border border-border-subtle rounded-md px-2 py-1 text-base text-text-primary"
          >
            {state.workspaces.map((w) => (
              <option key={w.id} value={w.id}>
                {formatAccountLabel(w, state.workspaces)}
              </option>
            ))}
          </select>
          {state.pendingWorkspaceSwitch && (
            <span
              className="ml-3 text-xs text-text-muted"
              data-testid="opencode-account-switch-pending"
            >
              Switching…
            </span>
          )}
        </div>
      )}
      <div className="flex gap-3">
        <button
          type="button"
          onClick={startSignIn}
          data-testid="opencode-sign-in-again"
          className="px-4 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30"
        >
          Sign in again
        </button>
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
      </div>
    </>
  );
}

/**
 * Formats a single workspace entry as a human-friendly account label.
 * Slot 0 of the picker (the OAuth-scoped user identity from
 * `/api/user`) is rendered as "Personal — {email}". Slots 1+ (org
 * workspaces from `/api/orgs`) are rendered as "Organization —
 * {name}". Single-workspace users see just one row.
 */
function formatAccountLabel(
  workspace: OpenCodeWorkspace,
  workspaces: OpenCodeWorkspace[],
): string {
  const isPersonal = workspaces.length > 0 && workspaces[0].id === workspace.id;
  return isPersonal
    ? `Personal — ${workspace.name}`
    : `Organization — ${workspace.name}`;
}

/* ── error ────────────────────────────────────────────────────────────── */

function ErrorView({
  message,
  startSignIn,
}: {
  message: string;
  startSignIn: () => void;
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
        onClick={startSignIn}
        data-testid="opencode-retry"
        className="px-4 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30"
      >
        Retry sign-in
      </button>
    </>
  );
}
