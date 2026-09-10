import { useEffect, useState } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import { useUpdateCheck, type UpdatePhase } from '../../hooks/useUpdateCheck';

// Settings > About section (issue #1526 + #1654 review).
//
// The updater state machine lives in `useUpdaterStore`; this section
// is the manual surface that drives it. The version row always
// renders; the manual check button only renders when the updater is
// enabled in this build (dev profile, non-Tauri page load, or
// non-production builds see a disabled notice instead — finding 3).
//
// The auto-launch prompt (`<UpdatePrompt />`) and this section
// subscribe to the same store, so a "Check for updates" click in
// Settings followed by a launch-time prompt is impossible to desync.
// The button is also disabled while a download or install is in
// flight so a mid-download click can't bump the seq guard and orphan
// the progress callbacks (finding 7).
//
// `current` and `unreachable` are surfaced HERE (not in `UpdatePrompt`)
// because they're informational rather than nag-worthy. The
// auto-launch prompt suppresses them so a launching app doesn't
// bother the user with "you're up to date" noise.

export function UpdateAboutSection() {
  const { state, enabled, check, install, retry, restart, dismiss } = useUpdateCheck();
  const [appVersion, setAppVersion] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void getVersion()
      .then((v) => {
        if (!cancelled) setAppVersion(v);
      })
      .catch((e: unknown) => {
        console.warn('[UpdateAbout] getVersion failed:', e);
        if (!cancelled) setAppVersion(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div className="pt-6 border-t border-border-subtle space-y-4" data-testid="settings-about-section">
      <h3 className="text-lg font-medium text-text-secondary">About</h3>
      <div className="text-base text-text-muted">
        <span className="text-text-secondary font-medium">Buildmesh</span>{' '}
        <span data-testid="settings-about-version">{appVersion ?? '…'}</span>
      </div>
      <div className="flex items-center gap-3">
        {/* Round-2 minor 1 — render a neutral "checking…" while the
            boot probe is in flight so the user doesn't briefly see
            "Update checks are disabled in this build" on every
            Settings open (the pre-fix behavior was to collapse the
            tri-state to false at the hook boundary, then surface a
            false-disabled flicker). */}
        {enabled === null ? (
          <p
            className="text-sm text-text-muted"
            data-testid="settings-about-loading"
          >
            Checking update support…
          </p>
        ) : enabled ? (
          <CheckButton state={state} onCheck={() => void check()} />
        ) : (
          <p
            className="text-sm text-text-muted"
            data-testid="settings-about-disabled"
          >
            Update checks are disabled in this build.
          </p>
        )}
        <UpdateStatusLine state={state} />
      </div>
      <UpdateStateActions
        state={state}
        onInstall={install}
        onRetry={retry}
        onRestart={restart}
        onDismiss={dismiss}
      />
    </div>
  );
}

// Disabled while checking / downloading / installing / ready_to_restart
// — a click during any of those phases would bump the seq guard and
// orphan in-flight or staged work (finding 7, round-2 minor 4).
function CheckButton({ state, onCheck }: { state: UpdatePhase; onCheck: () => void }) {
  const busy =
    state.kind === 'checking' ||
    state.kind === 'downloading' ||
    state.kind === 'installing' ||
    state.kind === 'ready_to_restart';
  const label: string =
    state.kind === 'checking' ? 'Checking…'
    : state.kind === 'downloading' ? 'Downloading…'
    : state.kind === 'installing' ? 'Installing…'
    : state.kind === 'ready_to_restart' ? 'Restart ready'
    : 'Check for updates';
  return (
    <button
      type="button"
      onClick={onCheck}
      disabled={busy}
      className="px-4 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50"
      data-testid="settings-about-check"
    >
      {label}
    </button>
  );
}

function UpdateStatusLine({ state }: { state: UpdatePhase }) {
  let label: string;
  let tone: 'muted' | 'success' | 'warning' = 'muted';
  let ariaLive: 'polite' | 'off' = 'off';

  switch (state.kind) {
    case 'idle':
      label = 'No update check has been run this session.';
      break;
    case 'checking':
      label = 'Checking for updates…';
      ariaLive = 'polite';
      break;
    case 'current':
      label = 'You’re up to date.';
      tone = 'success';
      ariaLive = 'polite';
      break;
    case 'unreachable':
      label = `Couldn’t reach the update feed. ${state.error}`;
      tone = 'warning';
      ariaLive = 'polite';
      break;
    case 'available':
      label = state.summary.message;
      break;
    case 'downloading':
      label = `Downloading v${state.summary.version}…`;
      ariaLive = 'polite';
      break;
    case 'installing':
      label = `Installing v${state.summary.version}…`;
      ariaLive = 'polite';
      break;
    case 'ready_to_restart':
      label = `Buildmesh ${state.summary.version} is ready to install.`;
      break;
    case 'failed':
      label = `Update failed (${state.failedAt}): ${state.error}`;
      tone = 'warning';
      ariaLive = 'polite';
      break;
  }

  const toneClass =
    tone === 'success'
      ? 'text-status-success'
      : tone === 'warning'
      ? 'text-status-warning'
      : 'text-text-muted';

  return (
    <p
      className={`text-sm ${toneClass}`}
      data-testid="settings-about-status"
      aria-live={ariaLive}
    >
      {label}
    </p>
  );
}

function UpdateStateActions({
  state,
  onInstall,
  onRetry,
  onRestart,
  onDismiss,
}: {
  state: UpdatePhase;
  onInstall: () => Promise<void>;
  onRetry: () => Promise<void>;
  onRestart: () => Promise<void>;
  onDismiss: () => void;
}) {
  if (state.kind === 'available') {
    return (
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={onInstall}
          className="px-4 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30"
          data-testid="settings-about-install"
        >
          Install v{state.summary.version} & Restart
        </button>
        <button
          type="button"
          onClick={onDismiss}
          className="px-3 py-2 text-base text-text-secondary hover:text-text-primary"
        >
          Dismiss
        </button>
      </div>
    );
  }
  if (state.kind === 'ready_to_restart') {
    return (
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={onRestart}
          className="px-4 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30"
          data-testid="settings-about-restart"
        >
          Restart now
        </button>
        <button
          type="button"
          onClick={onDismiss}
          className="px-3 py-2 text-base text-text-secondary hover:text-text-primary"
        >
          Later
        </button>
      </div>
    );
  }
  if (state.kind === 'failed') {
    return (
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={onRetry}
          className="px-4 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30"
          data-testid="settings-about-retry"
        >
          Retry
        </button>
        <button
          type="button"
          onClick={onDismiss}
          className="px-3 py-2 text-base text-text-secondary hover:text-text-primary"
        >
          Dismiss
        </button>
      </div>
    );
  }
  return null;
}