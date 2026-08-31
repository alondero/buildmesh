import { useEffect, useState } from 'react';
import type { SemanticTurnPayload } from '../../types/generated/SemanticTurnPayload';
import { diffNodeAgainstBase } from '../../lib/tauri';

interface SemanticTurnBannerProps {
  turn: SemanticTurnPayload;
  isActive: boolean;
  onResolve: (data: string) => void;
}

function isTextEntry(target: EventTarget | null): boolean {
  return target instanceof HTMLElement
    && (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName));
}

export function SemanticTurnBanner({ turn, isActive, onResolve }: SemanticTurnBannerProps) {
  const [diffCounts, setDiffCounts] = useState<{ additions: number; deletions: number } | null>(null);

  useEffect(() => {
    if (turn.kind !== 'turn_finished') {
      setDiffCounts(null);
      return;
    }
    const controller = new AbortController();
    void diffNodeAgainstBase(turn.node_id, controller.signal)
      .then((result) => {
        if (controller.signal.aborted) return;
        setDiffCounts(result.files.reduce(
          (totals, file) => ({
            additions: totals.additions + file.additions,
            deletions: totals.deletions + file.deletions,
          }),
          { additions: 0, deletions: 0 },
        ));
      })
      .catch(() => {
        // Diff stats are enrichment; the action remains usable when the repo
        // is unavailable or the node has already been removed.
      });
    return () => controller.abort();
  }, [turn.kind, turn.node_id]);

  useEffect(() => {
    if (!isActive) return;

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.repeat || event.altKey || event.ctrlKey || event.metaKey || isTextEntry(event.target)) {
        return;
      }

      const key = event.key.toLowerCase();
      let data: string | null = null;
      if (key === 'enter') data = turn.kind === 'turn_finished' ? '\r' : 'y\r';
      if (turn.kind !== 'turn_finished' && key === 'y') data = 'y\r';
      if (turn.kind !== 'turn_finished' && key === 'n') data = 'n\r';
      if (data === null) return;

      event.preventDefault();
      event.stopPropagation();
      onResolve(data);
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isActive, onResolve, turn.kind]);

  const isFinished = turn.kind === 'turn_finished';
  const approveLabel = turn.kind === 'command_confirmation' ? 'Approve (Y)' : 'Allow (Y)';
  const rejectLabel = turn.kind === 'command_confirmation' ? 'Deny (N)' : 'Reject (N)';
  const resolve = (data: string) => (event: React.MouseEvent<HTMLButtonElement>) => {
    event.stopPropagation();
    onResolve(data);
  };

  return (
    <div
      role="status"
      aria-live="polite"
      className="flex min-h-9 shrink-0 items-center gap-2 border-b border-accent-amber/35 bg-accent-amber/15 px-2 py-1 text-xs text-text-primary"
    >
      <span className="min-w-0 flex-1 truncate font-medium" title={turn.description}>
        {turn.description}{diffCounts && ` (+${diffCounts.additions} -${diffCounts.deletions})`}
      </span>
      <div className="flex shrink-0 items-center gap-1">
        {isFinished ? (
          <button
            type="button"
            onClick={resolve('\r')}
            className="rounded-sm border border-accent-amber/50 bg-bg-card px-2 py-1 font-semibold text-accent-amber hover:bg-accent-amber/15 focus-visible:outline-2 focus-visible:outline-accent-cyan"
          >
            Continue (Enter)
          </button>
        ) : (
          <>
            <button
              type="button"
              onClick={resolve('y\r')}
              className="rounded-sm border border-accent-green/50 bg-bg-card px-2 py-1 font-semibold text-accent-green hover:bg-accent-green/10 focus-visible:outline-2 focus-visible:outline-accent-cyan"
            >
              {approveLabel}
            </button>
            <button
              type="button"
              onClick={resolve('n\r')}
              className="rounded-sm border border-status-error/50 bg-bg-card px-2 py-1 font-semibold text-status-error hover:bg-status-error-bg focus-visible:outline-2 focus-visible:outline-accent-cyan"
            >
              {rejectLabel}
            </button>
          </>
        )}
      </div>
    </div>
  );
}
