import { useEffect } from 'react';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { useCloneActivityStore } from '../../stores/cloneActivityStore';
import { useUIStore } from '../../stores/uiStore';
import { enterMesh } from '../../lib/navigation';
import { formatElapsed } from '../../lib/nodeGlance';
import { useTickingNow } from '../../hooks/useTickingNow';

const SECTION_LABEL = 'text-2xs uppercase tracking-wide text-text-muted mb-1.5';

/**
 * The durable view of one clone operation (issue #2795).
 *
 * It is reachable at any time from the compact activity entry, and it is
 * the surface that answers the three questions a user has while a clone
 * runs: is it still running, what is it cloning, and how do I get back to
 * it. While the clone runs it shows what the backend actually knows —
 * that it is running, for how long, and into which destination. There is no
 * percentage and no phase, because the clone backend emits no progress
 * events: a bar that pretended to know more would be lying.
 */
export function CloneProgressModal({
  operationId,
  onClose,
}: {
  operationId: string;
  onClose: () => void;
}) {
  const operation = useCloneActivityStore(
    s => s.operations.find(op => op.id === operationId) ?? null
  );
  const retryClone = useCloneActivityStore(s => s.retryClone);
  const claimOperation = useCloneActivityStore(s => s.claimOperation);
  const releaseOperation = useCloneActivityStore(s => s.releaseOperation);

  const running = operation?.status === 'running';
  const now = useTickingNow(running);
  // The elapsed time is measured to the moment the clone SETTLED, not to the
  // moment this dialog opened: a clone that finished five seconds ago and is
  // opened an hour later took five seconds, and saying "Took 1h" would
  // report a duration the backend never had.
  const elapsed = operation
    ? (operation.finishedAt ?? now) - operation.startedAt
    : 0;

  // While this surface shows the operation, its outcome is reported here
  // rather than as a toast the user is already looking at.
  useEffect(() => {
    claimOperation(operationId);
    return () => releaseOperation(operationId);
  }, [operationId, claimOperation, releaseOperation]);

  // The operation was dismissed from the activity entry while this dialog
  // was open — there is nothing left to show, so close.
  useEffect(() => {
    if (!operation) onClose();
  }, [operation, onClose]);

  if (!operation) return null;

  const title =
    operation.status === 'running'
      ? 'Clone in progress'
      : operation.status === 'succeeded'
        ? 'Clone complete'
        : 'Clone failed';

  const handleRetry = () => {
    const next = retryClone(operation.id);
    if (next) useUIStore.getState().openCloneProgress(next.operation.id);
  };

  const handleOpenMesh = () => {
    if (operation.meshId !== null) enterMesh(operation.meshId);
    onClose();
  };

  return (
    <Modal
      onClose={onClose}
      labelledBy="clone-progress-title"
      maxWidth="max-w-md"
      closeOnBackdrop={false}
    >
      <div className="flex items-start justify-between mb-4">
        <h2 id="clone-progress-title" className="text-sm font-semibold text-text-primary">
          {title}
        </h2>
        {/* While the clone runs this control HIDES the progress — it does
            not cancel anything, so it must not be labelled "Close". */}
        <ModalCloseButton onClose={onClose} label={running ? 'Hide progress' : 'Close'} />
      </div>

      {running && (
        <div
          role="progressbar"
          aria-label="Clone in progress"
          className="h-1 w-full overflow-hidden rounded-full bg-bg-card mb-4"
        >
          {/* Indeterminate: the backend reports no transfer progress, so
              the bar only says "working". */}
          <div className="h-full w-1/3 animate-pulse-fast rounded-full bg-accent-cyan" />
        </div>
      )}

      <div className="mb-4">
        <div className={SECTION_LABEL}>Repository</div>
        <p className="text-xs text-text-primary break-all">{operation.repo}</p>
      </div>

      <div className="mb-4">
        <div className={SECTION_LABEL}>Destination</div>
        <p className="text-xs font-mono text-text-secondary bg-bg-card border border-border-subtle rounded-md px-2 py-1 break-all">
          {operation.destination}
        </p>
      </div>

      <div className="mb-4">
        <div className={SECTION_LABEL}>
          {operation.status === 'running' ? 'Elapsed' : 'Took'}
        </div>
        <p className="text-xs text-text-secondary" data-testid="clone-progress-elapsed">
          {formatElapsed(elapsed)}
        </p>
      </div>

      {operation.status === 'failed' && (
        <p role="alert" className="mb-3 text-xs text-status-error break-words">
          {operation.error}
        </p>
      )}

      {running && (
        <p role="status" className="mb-3 text-xs text-text-secondary">
          The clone keeps running if you close this dialog. Nothing here stops it.
        </p>
      )}

      {operation.status === 'succeeded' && (
        <p role="status" className="mb-3 text-xs text-text-secondary">
          Cloned into {operation.destination}.
        </p>
      )}

      <div className="flex justify-end gap-2">
        {operation.status === 'failed' && (
          <button
            type="button"
            onClick={handleRetry}
            className="px-3 py-1.5 text-xs font-medium text-text-primary bg-bg-selection hover:bg-bg-card-hover border border-accent-cyan rounded-md transition-colors"
          >
            Retry
          </button>
        )}
        {operation.status === 'succeeded' && (
          <button
            type="button"
            onClick={handleOpenMesh}
            className="px-3 py-1.5 text-xs font-medium text-text-primary bg-bg-selection hover:bg-bg-card-hover border border-accent-cyan rounded-md transition-colors"
          >
            Open mesh
          </button>
        )}
        <button
          type="button"
          onClick={onClose}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          {running ? 'Hide progress' : 'Close'}
        </button>
      </div>
    </Modal>
  );
}
