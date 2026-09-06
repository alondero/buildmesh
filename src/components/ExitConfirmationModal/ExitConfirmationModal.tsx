import { useRef } from 'react';
import { Modal } from '../shared/Modal';
import { formatExitBody, type ExitNonResumableEntry } from '../../lib/exitGuard';

interface ExitConfirmationModalProps {
  activeCount: number;
  nonResumable: ExitNonResumableEntry[];
  exiting?: boolean;
  onKeepWorking: () => void;
  onExit: () => void;
}

const noop = () => {};

/**
 * Exit confirmation modal (issue #1501).
 *
 * Rendered by `WindowCloseGuard` when a window close is requested while
 * active agent sessions exist. "Keep Working" is the default focused
 * action (safe); "Exit Buildmesh" is destructive and triggers the
 * backend's graceful shutdown sweep (mark suspended + kill sessions).
 */
export function ExitConfirmationModal({
  activeCount,
  nonResumable,
  exiting = false,
  onKeepWorking,
  onExit,
}: ExitConfirmationModalProps) {
  const keepWorkingRef = useRef<HTMLButtonElement>(null);

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
        Exit Buildmesh?
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
            Exiting will permanently terminate their progress:
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
          className="px-3 py-1.5 text-sm text-white bg-status-error/80 hover:bg-status-error rounded-md transition-colors disabled:opacity-50"
        >
          {exiting ? 'Exiting…' : 'Exit Buildmesh'}
        </button>
      </div>
    </Modal>
  );
}
