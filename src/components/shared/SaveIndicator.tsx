/**
 * SaveIndicator — `min-h-7` slot keeps the surrounding UI from
 * reflowing when the status flips between `error` and `idle`. Lifted
 * out of `MeshPropertiesTab` (issue #729) so `ScratchpadTab` adopts
 * the same surface (issue #813).
 */
import type { SaveStatus } from '../../hooks/useSaveStatus';

export interface SaveIndicatorProps {
  status: SaveStatus;
  /** Rejection reason for `status === 'error'`; `null` otherwise. */
  error: string | null;
  /** Called when the user clicks the ✕ on the error row. Only the
   *  `error` state renders the dismiss button; the hook resets itself
   *  on the next `start()` regardless, so wiring this is optional
   *  but keeps a slow IPC's error visible until acknowledged. */
  onDismiss?: () => void;
  /** `data-testid` — defaults to `save-indicator` to keep the
   *  existing test selectors stable after extraction. */
  testId?: string;
}

export function SaveIndicator({
  status,
  error,
  onDismiss,
  testId = 'save-indicator',
}: SaveIndicatorProps) {
  return (
    <div
      role="status"
      aria-live="polite"
      data-testid={testId}
      className="min-h-7 flex items-center justify-between text-xs rounded-md px-2 py-1"
    >
      {status === 'saving' && <span className="text-text-muted">Saving…</span>}
      {status === 'saved' && <span className="text-status-success">Saved</span>}
      {status === 'error' && (
        <>
          <span className="text-status-error break-words flex-1">
            Save failed{error ? `: ${error}` : ''}
          </span>
          {onDismiss && (
            <button
              type="button"
              onClick={onDismiss}
              aria-label="Dismiss save error"
              className="ml-2 text-status-error/70 hover:text-status-error text-[11px]"
            >
              ✕
            </button>
          )}
        </>
      )}
    </div>
  );
}
