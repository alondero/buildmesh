import { useState, type RefObject } from 'react';
import { addToast } from '../../stores/toastStore';
import { formatError } from '../../lib/errorUtils';

interface MeshCreateFailurePanelProps {
  /**
   * The message the backend produced. The panel renders the message it is
   * given, so the store's error text reaches the user as it is —
   * `tests/unit/mesh-create-modal.test.tsx` ("shows the backend failure
   * instead of the generic logs message") pins that, because the dialog used
   * to replace this cause with generic copy (issue #2794).
   */
  message: string;
  /** One short sentence naming the cause and the next step. */
  hint?: string;
  /**
   * The diagnostic block for the expandable detail area. It is the text the
   * Copy button puts on the clipboard.
   */
  detail?: string;
  /** Offer to run the same request again. */
  onRetry?: () => void;
  /** Offer to pick a different repository folder. */
  onChangeFolder?: () => void;
  /**
   * The panel itself. It is focusable (but not tabbable) so the dialog can
   * move focus onto the failure: the submit control that failed is disabled
   * while the request runs, and a disabled control does not hold the focus,
   * so without this the focus would fall out of the dialog onto the page
   * behind it. `tests/unit/mesh-create-modal.test.tsx` ("announces the failure
   * accessibly and keeps focus in the dialog") pins that.
   */
  panelRef?: RefObject<HTMLDivElement | null>;
}

/**
 * The inline failure surface of the New mesh dialog (issue #2794).
 *
 * It shows the backend's own message, adds one short sentence naming the next
 * step, keeps the full diagnostic block in an expandable and copyable area,
 * and offers the recovery the cause supports — Retry for a failure that may
 * pass on its own, Choose another folder for a path or access failure, and
 * both when the cause does not match a known one (the mapping lives in
 * `src/lib/meshCreateFailure.ts`).
 *
 * `role="alert"` announces the failure as it appears, so the message reaches a
 * screen-reader user without them having to find it. The dialog moves focus
 * onto the panel as well, because the control that failed is disabled while
 * the request runs and a disabled control does not hold the focus.
 */
export function MeshCreateFailurePanel({
  message,
  hint,
  detail,
  onRetry,
  onChangeFolder,
  panelRef,
}: MeshCreateFailurePanelProps) {
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'failed'>('idle');

  const copyDetail = async () => {
    if (!detail) return;
    try {
      await navigator.clipboard?.writeText(detail);
      setCopyState('copied');
    } catch (e) {
      setCopyState('failed');
      addToast('New mesh', `Couldn't copy the details: ${formatError(e)}`, 'error');
    }
  };

  return (
    <div
      ref={panelRef}
      tabIndex={-1}
      role="alert"
      aria-live="assertive"
      data-testid="mesh-create-failure"
      className="mb-3 rounded-md border border-status-error/40 bg-status-error/10 px-2.5 py-2 focus:outline-none"
    >
      <p
        className="text-xs text-status-error break-words"
        data-testid="mesh-create-failure-message"
      >
        {message}
      </p>
      {hint && (
        <p className="mt-1 text-2xs text-text-secondary break-words" data-testid="mesh-create-failure-hint">
          {hint}
        </p>
      )}
      {detail && (
        <details className="mt-1.5 text-2xs text-text-muted">
          <summary className="cursor-pointer min-h-[20px]">Details</summary>
          <pre
            className="mt-1 whitespace-pre-wrap break-words font-mono text-text-secondary"
            data-testid="mesh-create-failure-detail"
          >
            {detail}
          </pre>
          <button
            type="button"
            onClick={() => void copyDetail()}
            className="mt-1 px-2 py-1 text-2xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
          >
            {copyState === 'copied' ? 'Copied' : 'Copy details'}
          </button>
        </details>
      )}
      {(onRetry || onChangeFolder) && (
        <div className="mt-2 flex flex-wrap gap-2">
          {onRetry && (
            <button
              type="button"
              onClick={onRetry}
              data-testid="mesh-create-retry"
              className="px-2.5 py-1 text-2xs font-medium text-text-primary bg-bg-selection hover:bg-bg-card-hover border border-accent-cyan rounded-md transition-colors"
            >
              Retry
            </button>
          )}
          {onChangeFolder && (
            <button
              type="button"
              onClick={onChangeFolder}
              data-testid="mesh-create-change-folder"
              className="px-2.5 py-1 text-2xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
            >
              Choose another folder…
            </button>
          )}
        </div>
      )}
    </div>
  );
}
