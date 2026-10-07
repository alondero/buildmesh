import { useReducer, useRef } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import * as api from '../../lib/tauri';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import {
  errorMessageFromUnknown,
  opencodeAccountReducer,
  type State,
} from './OpenCodeAccountCard.reducer';
import { StateBody } from './OpenCodeAccountViews';

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
