import { useEffect } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import * as api from '../../lib/tauri';
import type { OpenCodeWorkspace } from '../../types/generated/OpenCodeWorkspace';
import {
  errorMessageFromUnknown,
  type Action,
  type State,
} from './OpenCodeAccountCard.reducer';

/**
 * The `awaitingActivation` branch of the OpenCode Console card. Split out of
 * `OpenCodeAccountCard.tsx` (issue #1880) because it is the only view that
 * owns an effect-driven `setInterval` polling loop — keeping it apart leaves
 * the remaining views purely presentational. Pure move.
 */
export function AwaitingActivationView({
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
          className="text-base text-text-secondary hover:text-text-primary"
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
