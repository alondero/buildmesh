import { Modal } from '../shared/Modal';
import { useUpdateCheck, type UpdatePhase } from '../../hooks/useUpdateCheck';

// Updater prompt (issue #1526).
//
// The previous `UpdatePrompt` had three states (visible / hidden /
// installing) and no failure surface. The new one renders nothing for
// `idle` / `checking` / `current` / `unreachable` (those are Settings >
// About's concern, not a launched-app nag) and exposes a phase-aware
// body for `available` / `downloading` / `installing` /
// `ready_to_restart` / `failed`.
//
// The Settings > About section subscribes to the same hook so a
// dismissed prompt doesn't leave stale state behind — but `UpdatePrompt`
// only mounts once at App root, so its presence is purely a
// "should we nag on launch" question. The shared state lives in the
// hook.
//
// aria-live regions name the phase so a screen reader user hears
// progress ("Checking for updates…", "Downloading v0.3.0…", "Restart
// ready"). Inline failures are sanitized (the updater lib strips
// URLs / tokens / paths before they reach this layer) and offer Retry.

const RELEASE_FALLBACK_URL = 'https://github.com/alondero/buildmesh/releases';

interface ProgressVisual {
  label: string;
  pct: number | null; // null = indeterminate
}

function describeProgress(
  state: Extract<UpdatePhase, { kind: 'downloading' }>,
): ProgressVisual {
  const { downloaded, total } = state.progress;
  if (total === null || total === 0) {
    return { label: formatBytes(downloaded), pct: null };
  }
  const ratio = Math.min(1, downloaded / total);
  return { label: `${formatBytes(downloaded)} / ${formatBytes(total)}`, pct: ratio };
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

export function UpdatePrompt() {
  const { state, install, retry, restart, dismiss } = useUpdateCheck();

  // Settings > About owns the manual surface; this prompt only appears
  // for the launch-time nag and the install lifecycle.
  if (
    state.kind === 'idle' ||
    state.kind === 'checking' ||
    state.kind === 'current' ||
    state.kind === 'unreachable'
  ) {
    return null;
  }

  if (state.kind === 'available') {
    return <AvailablePrompt summary={state.summary} onInstall={install} onDismiss={dismiss} />;
  }
  if (state.kind === 'downloading' || state.kind === 'installing') {
    return <ProgressPrompt phase={state} />;
  }
  if (state.kind === 'ready_to_restart') {
    return <ReadyPrompt summary={state.summary} onRestart={restart} onDismiss={dismiss} />;
  }
  // failed
  return (
    <FailedPrompt
      summary={state.summary}
      failedAt={state.failedAt}
      error={state.error}
      onRetry={retry}
      onDismiss={dismiss}
    />
  );
}

function AvailablePrompt({
  summary,
  onInstall,
  onDismiss,
}: {
  summary: { version: string; notes: string; message: string };
  onInstall: () => Promise<void>;
  onDismiss: () => void;
}) {
  return (
    <Modal onClose={onDismiss} labelledBy="update-prompt-title" maxWidth="max-w-sm">
      <h2 id="update-prompt-title" className="text-sm font-semibold text-text-primary mb-2">
        Update available
      </h2>
      <p className="text-xs text-text-muted mb-3" aria-live="polite">
        {summary.message}
      </p>
      {summary.notes && (
        <pre className="text-xs text-text-secondary bg-bg-base/60 border border-border-subtle rounded-md p-2 mb-5 max-h-40 overflow-auto whitespace-pre-wrap">
          {summary.notes}
        </pre>
      )}
      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onDismiss}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          Later
        </button>
        <button
          type="button"
          onClick={() => void onInstall()}
          className="px-3 py-1.5 text-xs font-medium rounded-md bg-accent-cyan/10 text-accent-cyan border border-accent-cyan/20 hover:bg-accent-cyan/20 transition-colors"
          data-testid="update-prompt-install"
        >
          Install & Restart
        </button>
      </div>
    </Modal>
  );
}

function ProgressPrompt({
  phase,
}: {
  phase: Extract<UpdatePhase, { kind: 'downloading' | 'installing' }>;
}) {
  const isDownloading = phase.kind === 'downloading';
  const title = isDownloading ? `Downloading v${phase.summary.version}…` : `Installing v${phase.summary.version}…`;
  const visual = isDownloading ? describeProgress(phase) : null;
  // Once download/install starts, the work is non-cancellable — closing
  // the modal would leave the user unsure whether the install is still
  // running. Disable the close path so the only way out is to let it
  // finish (or hit Retry on a failure).
  const noop = () => {};
  return (
    <Modal onClose={noop} labelledBy="update-prompt-title" maxWidth="max-w-sm" closeOnBackdrop={false}>
      <h2 id="update-prompt-title" className="text-sm font-semibold text-text-primary mb-2">
        Update in progress
      </h2>
      <p className="text-xs text-text-muted mb-3" aria-live="polite" data-testid="update-prompt-status">
        {title}
      </p>
      {visual && (
        <div className="mb-5">
          <div
            role="progressbar"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={visual.pct === null ? undefined : Math.round(visual.pct * 100)}
            aria-label={title}
            className="h-1.5 w-full rounded-full bg-bg-base/60 overflow-hidden border border-border-subtle"
          >
            <div
              className="h-full bg-accent-cyan transition-all"
              style={{ width: visual.pct === null ? '40%' : `${visual.pct * 100}%` }}
            />
          </div>
          <p className="mt-1 text-xs text-text-muted">{visual.label}</p>
        </div>
      )}
      {/* The close "×" is rendered by the Modal header; with
          closeOnBackdrop=false the user can still Escape. The footer
          note explains why — silence feels like a freeze. */}
      <p className="text-xs text-text-muted">Keep this window open until the update finishes.</p>
    </Modal>
  );
}

function ReadyPrompt({
  summary,
  onRestart,
  onDismiss,
}: {
  summary: { version: string; notes: string; message: string };
  onRestart: () => Promise<void>;
  onDismiss: () => void;
}) {
  return (
    <Modal onClose={onDismiss} labelledBy="update-prompt-title" maxWidth="max-w-sm">
      <h2 id="update-prompt-title" className="text-sm font-semibold text-text-primary mb-2">
        Restart ready
      </h2>
      <p className="text-xs text-text-muted mb-5" aria-live="polite" data-testid="update-prompt-status">
        Buildmesh {summary.version} is installed. Restart to apply.
      </p>
      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onDismiss}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
          data-testid="update-prompt-later"
        >
          Later
        </button>
        <button
          type="button"
          onClick={() => void onRestart()}
          className="px-3 py-1.5 text-xs font-medium rounded-md bg-accent-cyan/10 text-accent-cyan border border-accent-cyan/20 hover:bg-accent-cyan/20 transition-colors"
          data-testid="update-prompt-restart"
        >
          Restart now
        </button>
      </div>
    </Modal>
  );
}

function FailedPrompt({
  summary,
  failedAt,
  error,
  onRetry,
  onDismiss,
}: {
  summary: { version: string; notes: string; message: string } | null;
  failedAt: 'check' | 'download' | 'install';
  error: string;
  onRetry: () => Promise<void>;
  onDismiss: () => void;
}) {
  const phaseLabel =
    failedAt === 'check' ? 'Checking for updates' :
    failedAt === 'download' ? 'Downloading the update' :
    'Installing the update';
  return (
    <Modal onClose={onDismiss} labelledBy="update-prompt-title" maxWidth="max-w-sm">
      <h2 id="update-prompt-title" className="text-sm font-semibold text-text-primary mb-2">
        Update failed
      </h2>
      <p className="text-xs text-text-muted mb-3" aria-live="assertive">
        {phaseLabel} failed: {error}
      </p>
      <p className="text-xs text-text-muted mb-5">
        You can retry, or download the latest release directly from GitHub
        and install it manually.
      </p>
      <div className="flex justify-end gap-2">
        <a
          href={RELEASE_FALLBACK_URL}
          target="_blank"
          rel="noreferrer"
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
          data-testid="update-prompt-fallback"
        >
          Open GitHub releases
        </a>
        <button
          type="button"
          onClick={onDismiss}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          Dismiss
        </button>
        <button
          type="button"
          onClick={() => void onRetry()}
          className="px-3 py-1.5 text-xs font-medium rounded-md bg-accent-cyan/10 text-accent-cyan border border-accent-cyan/20 hover:bg-accent-cyan/20 transition-colors"
          data-testid="update-prompt-retry"
        >
          Retry
        </button>
      </div>
      {summary && (
        <p className="mt-3 text-xs text-text-muted" data-testid="update-prompt-failed-summary">
          Target version: {summary.version}
        </p>
      )}
    </Modal>
  );
}