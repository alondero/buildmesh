import { useRef, useState } from 'react';
import * as api from '../../lib/tauri';
import type { OpenCodeWorkspace } from '../../types/generated/OpenCodeWorkspace';
import { addToast } from '../../stores/toastStore';
import {
  errorMessageFromUnknown,
  type Action,
  type State,
} from './OpenCodeAccountCard.reducer';
import { AwaitingActivationView } from './OpenCodeAwaitingActivationView';

/**
 * Presentational sub-views of the OpenCode Console settings card, split out
 * of `OpenCodeAccountCard.tsx` (issue #1880) so that file can stay the owner
 * of the device-flow state machine while this one owns the markup for each
 * reducer sub-state. Pure move — no behaviour lives here that did not live in
 * the parent. `AwaitingActivationView` carries its own polling effect and sits
 * in a sibling module because it is the only view that drives a timer.
 */
export function StateBody({
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
        className="px-5 py-2.5 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover"
      >
        Sign in with OpenCode Console
      </button>
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
          className="px-4 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover"
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
        className="px-4 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover"
      >
        Retry sign-in
      </button>
    </>
  );
}
