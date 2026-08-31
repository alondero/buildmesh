import { useEffect, useState } from 'react';
import type { KeyboardEvent } from 'react';
import type { SemanticTurnPayload } from '../../types/generated/SemanticTurnPayload';
import { diffNodeAgainstBase } from '../../lib/tauri';

interface SemanticTurnBannerProps {
  turn: SemanticTurnPayload;
  isActive: boolean;
  onResolve: (data: string) => void | Promise<void>;
  onFinish: () => void | Promise<void>;
}

export function SemanticTurnBanner({ turn, isActive, onResolve, onFinish }: SemanticTurnBannerProps) {
  const [diffCounts, setDiffCounts] = useState<{ additions: number; deletions: number } | null>(null);
  const [submitting, setSubmitting] = useState(false);

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

  const submit = (data: string | null) => {
    if (submitting) return;
    setSubmitting(true);
    void Promise.resolve(data === null ? onFinish() : onResolve(data));
  };
  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!isActive || submitting || event.repeat || event.altKey || event.ctrlKey || event.metaKey) return;
    if ((event.target as HTMLElement).tagName === 'BUTTON' && event.key === 'Enter') return;
      const key = event.key.toLowerCase();
      let data: string | null = null;
      if (key === 'enter') data = turn.kind === 'turn_finished' ? null : 'y\r';
      if (turn.kind !== 'turn_finished' && key === 'y') data = 'y\r';
      if (turn.kind !== 'turn_finished' && key === 'n') data = 'n\r';
      if (data === null && key !== 'enter') return;

      event.preventDefault();
      event.stopPropagation();
      submit(data);
  };

  const isFinished = turn.kind === 'turn_finished';
  const approveLabel = turn.kind === 'command_confirmation' ? 'Approve (Y)' : 'Allow (Y)';
  const rejectLabel = turn.kind === 'command_confirmation' ? 'Deny (N)' : 'Reject (N)';
  return (
    <div
      role="status"
      data-semantic-turn-banner
      tabIndex={0}
      onKeyDown={handleKeyDown}
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
            onClick={() => submit(null)}
            disabled={submitting}
            className="rounded-sm border border-accent-amber/50 bg-bg-card px-2 py-1 font-semibold text-accent-amber hover:bg-accent-amber/15 focus-visible:outline-2 focus-visible:outline-accent-cyan"
          >
            Continue (Enter)
          </button>
        ) : (
          <>
            <button
              type="button"
              onClick={() => submit('y\r')}
              disabled={submitting}
              className="rounded-sm border border-accent-green/50 bg-bg-card px-2 py-1 font-semibold text-accent-green hover:bg-accent-green/10 focus-visible:outline-2 focus-visible:outline-accent-cyan"
            >
              {approveLabel}
            </button>
            <button
              type="button"
              onClick={() => submit('n\r')}
              disabled={submitting}
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
