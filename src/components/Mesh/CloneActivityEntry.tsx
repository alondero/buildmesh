import { selectVisibleOperations, useCloneActivityStore } from '../../stores/cloneActivityStore';
import { useUIStore } from '../../stores/uiStore';
import { enterMesh } from '../../lib/navigation';
import { formatElapsed } from '../../lib/nodeGlance';
import { useTickingNow } from '../../hooks/useTickingNow';
import { Spinner } from '../shared/Spinner';

/**
 * One compact, durable entry for a clone operation (issue #2795).
 *
 * The toast that used to be the only trace of a dismissed clone expires;
 * this entry does not. It stays in the notification area while the clone
 * runs — so the user can still answer "is it still going?" — and it keeps
 * a settled clone reachable afterwards: a failure stays there long after
 * its toast is gone, with a route back to the progress view that can retry
 * it, and a success keeps its "Open mesh" route.
 *
 * It states only what the backend knows: that a clone is running, for how
 * long, and into which destination.
 */
function CloneActivityEntryCard({ operationId }: { operationId: string }) {
  const operation = useCloneActivityStore(
    s => s.operations.find(op => op.id === operationId) ?? null
  );
  const dismissOperation = useCloneActivityStore(s => s.dismissOperation);
  const running = operation?.status === 'running';
  const now = useTickingNow(running);

  if (!operation) return null;

  const elapsed = formatElapsed(
    (operation.finishedAt ?? now) - operation.startedAt
  );
  // Read once into a local: the mesh id is narrowed here, so the handler
  // below does not need a cast to say what it already knows.
  const meshId = operation.meshId;
  const label =
    operation.status === 'running'
      ? 'Cloning'
      : operation.status === 'succeeded'
        ? 'Cloned'
        : 'Clone failed';
  const tone =
    operation.status === 'running'
      ? 'text-accent-cyan'
      : operation.status === 'succeeded'
        ? 'text-status-success'
        : 'text-status-warning';
  const border =
    operation.status === 'succeeded'
      ? 'border-status-success/50'
      : operation.status === 'failed'
        ? 'border-status-warning/50'
        : 'border-border-strong';

  return (
    <div
      data-testid="clone-activity-entry"
      className={`animate-slide-in-right bg-bg-surface border ${border} rounded-md flex items-center gap-2 min-w-[280px] max-w-[420px] shadow-md px-3 py-2`}
    >
      <button
        type="button"
        onClick={() => useUIStore.getState().openCloneProgress(operation.id)}
        aria-label={`${label} ${operation.repo} into ${operation.destination} — open clone details`}
        className="flex-1 min-w-0 flex items-center gap-2 text-left rounded-md focus-visible:outline-2 focus-visible:outline-accent-cyan"
      >
        {running ? (
          <Spinner className="w-3 h-3 shrink-0" />
        ) : (
          <span
            aria-hidden="true"
            className={`shrink-0 text-xs font-bold ${tone}`}
          >
            {operation.status === 'succeeded' ? '✓' : '!'}
          </span>
        )}
        <span className="min-w-0 flex-1">
          <span className={`block text-2xs font-bold uppercase ${tone}`}>{label}</span>
          <span className="block text-xs text-text-secondary truncate">
            {operation.repo}
            {running ? ` · ${elapsed}` : ''}
          </span>
          {/* Where it is cloning: the destination is what tells two clones
              of the same repository apart, and it answers "where is it
              cloning?" without opening the progress surface. */}
          <span className="block text-2xs text-text-muted truncate" title={operation.destination}>
            {operation.destination}
          </span>
        </span>
      </button>
      {operation.status === 'succeeded' && meshId !== null && (
        <button
          type="button"
          onClick={() => enterMesh(meshId)}
          aria-label={`Open mesh ${operation.meshName ?? operation.repo}`}
          className="shrink-0 px-2 py-1 text-2xs font-medium text-accent-cyan hover:text-accent-cyan/80 hover:bg-bg-card-hover rounded-md transition-colors"
        >
          Open mesh
        </button>
      )}
      {operation.status !== 'running' && (
        <button
          type="button"
          onClick={() => dismissOperation(operation.id)}
          aria-label={`Dismiss clone activity for ${operation.repo}`}
          className="shrink-0 -m-1 p-1 rounded-md text-text-secondary hover:text-text-primary hover:bg-bg-card-hover text-base leading-none transition-colors"
        >
          ×
        </button>
      )}
    </div>
  );
}

/**
 * The activity entries for the live clone operations. Every operation gets
 * its own entry: two clones can run at once, and a clone with no entry of
 * its own would be invisible — the failure this surface exists to prevent.
 */
export function CloneActivityEntry() {
  const operations = useCloneActivityStore(s => s.operations);
  const visible = selectVisibleOperations(operations);

  return (
    <>
      {visible.map(operation => (
        <CloneActivityEntryCard key={operation.id} operationId={operation.id} />
      ))}
    </>
  );
}
