import { useRef } from 'react';
import { Modal } from '../shared/Modal';
import { formatExitBody, type ExitNonResumableEntry } from '../../lib/exitGuard';
import type { ExitPromptMode } from '../../stores/exitPromptStore';

interface ExitConfirmationModalProps {
  activeCount: number;
  nonResumable: ExitNonResumableEntry[];
  exiting?: boolean;
  /** Issue #1654 review finding 4 — the modal copy varies by which
   *  shutdown is being gated. `update-restart` reads "Restart
   *  Buildmesh?" with a "Restart now" CTA; `window-close` keeps the
   *  pre-existing "Exit Buildmesh?" / "Exit Buildmesh" copy so the
   *  window-close toast contract (and any consumers that pattern-match
   *  on its title) stays stable. */
  mode: ExitPromptMode;
  onKeepWorking: () => void;
  onExit: () => void;
}

const noop = () => {};

/**
 * Exit confirmation modal (issue #1501, review finding 4).
 *
 * Rendered by `WindowCloseGuard` when a window close is requested while
 * active agent sessions exist, OR by `useUpdaterStore.restart()` when
 * the user clicks "Restart now" on a staged update. "Keep Working" is
 * the default focused action (safe); the destructive CTA triggers the
 * action chosen by `mode` (`exit_application` for `window-close`,
 * `relaunch()` for `update-restart`).
 */
export function ExitConfirmationModal({
  activeCount,
  nonResumable,
  exiting = false,
  mode,
  onKeepWorking,
  onExit,
}: ExitConfirmationModalProps) {
  const keepWorkingRef = useRef<HTMLButtonElement>(null);

  // Mode-specific copy (finding 4): a "Restart now" user must not see
  // "Exit Buildmesh?" — that's the bug the reviewer flagged. The
  // window-close toast title (used by `confirmExit`'s failure path) is
  // intentionally NOT changed for stability, but the modal body here
  // reflects what the user actually clicked.
  const isRestart = mode === 'update-restart';
  const title = isRestart ? 'Restart Buildmesh?' : 'Exit Buildmesh?';
  const ctaLabel = exiting
    ? isRestart ? 'Restarting…' : 'Exiting…'
    : isRestart ? 'Restart now' : 'Exit Buildmesh';
  const ctaTestId = isRestart ? 'exit-confirm-restart' : 'exit-confirm-exit';

  // While destruction is in flight the modal is a progress surface, not a
  // dismissible dialog: Escape/backdrop route to a no-op and backdrop
  // clicks are disabled, so the modal can't vanish mid-destroy and leave
  // the app in a half-exited state (issue #1501 review).
  return (
    <Modal
      onClose={exiting ? noop : onKeepWorking}
      labelledBy="exit-confirm-title"
      maxWidth="max-w-md"
      closeOnBackdrop={!exiting}
      defaultFocusRef={keepWorkingRef}
    >
      <h2 id="exit-confirm-title" className="text-lg font-semibold text-text-primary mb-2">
        {title}
      </h2>
      <p className="text-sm text-text-secondary mb-4">{formatExitBody(activeCount)}</p>
      {nonResumable.length > 0 && (
        <div
          role="alert"
          className="mb-4 border border-status-warning/40 bg-status-warning/10 rounded-md px-4 py-3 text-status-warning"
        >
          <p className="text-sm mb-2">
            <span aria-hidden="true">⚠️ </span>
            The following session(s) do not support resumption or have not saved a session ID.
            {isRestart ? ' Restarting' : ' Exiting'} will permanently terminate their progress:
          </p>
          <ul className="list-disc list-inside text-sm space-y-0.5">
            {nonResumable.map((n) => (
              <li key={n.id}>
                {n.name} ({n.providerDisplay})
              </li>
            ))}
          </ul>
        </div>
      )}
      <div className="flex justify-end gap-2">
        <button
          ref={keepWorkingRef}
          type="button"
          onClick={onKeepWorking}
          disabled={exiting}
          className="px-3 py-1.5 text-sm text-text-primary bg-bg-card border border-border-subtle hover:border-border-default rounded-md transition-colors disabled:opacity-50"
        >
          Keep Working
        </button>
        <button
          type="button"
          onClick={onExit}
          disabled={exiting}
          data-testid={ctaTestId}
          className="px-3 py-1.5 text-sm text-white bg-status-error/80 hover:bg-status-error rounded-md transition-colors disabled:opacity-50"
        >
          {ctaLabel}
        </button>
      </div>
    </Modal>
  );
}
