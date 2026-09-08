import { useEffect, useState } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import { useUpdateCheck, type UpdatePhase } from '../../hooks/useUpdateCheck';

// Settings > About section (issue #1526).
//
// The updater state machine lives in `useUpdateCheck`; this section is
// the manual surface that drives it. It always renders, even when the
// updater is disabled in the current build (dev profile, non-Tauri
// page load) — the version row is always useful, and the manual check
// button gracefully reports "Update checks are disabled in this build".
//
// The auto-launch prompt (`<UpdatePrompt />`) and this section share
// the same hook, so a "Check for updates" click in Settings followed
// by a launch-time prompt is impossible to desync — both see the
// same `state`.
//
// `current` and `unreachable` are surfaced HERE (not in `UpdatePrompt`)
// because they're informational rather than nag-worthy. The
// auto-launch prompt suppresses them so a launching app doesn't
// bother the user with "you're up to date" noise.

interface UpdateAboutSectionProps {
  /** Render an "Open Settings" affordance only on the inline surface
   *  (the full Settings modal already is the settings). Currently
   *  unused but kept for future drill-in surfaces. */
  compact?: boolean;
}

export function UpdateAboutSection({ compact: _compact = false }: UpdateAboutSectionProps) {
  const { state, check, install, retry, restart, dismiss } = useUpdateCheck();
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
        <button
          type="button"
          onClick={() => void check()}
          disabled={state.kind === 'checking'}
          className="px-4 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50"
          data-testid="settings-about-check"
        >
          {state.kind === 'checking' ? 'Checking…' : 'Check for updates'}
        </button>
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