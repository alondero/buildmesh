import { useWorktreeClosePromptStore } from '../../stores/worktreeClosePromptStore';

export function WorktreeCloseDialog() {
  const pending = useWorktreeClosePromptStore(state => state.pending);
  const choose = useWorktreeClosePromptStore(state => state.choose);

  if (!pending) return null;

  const riskParts = [
    pending.safety.has_uncommitted ? 'uncommitted changes' : null,
    pending.safety.has_unpushed ? 'unpushed or unmerged commits' : null,
  ].filter(Boolean);

  const riskText = riskParts.length > 0 ? riskParts.join(' and ') : 'work that may not be recoverable';

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center" onClick={() => choose('cancel')}>
      <div className="absolute inset-0 bg-black/70" />
      <div
        className="relative bg-bg-overlay border border-border-default rounded-lg shadow-2xl p-6 max-w-md w-full"
        onClick={e => e.stopPropagation()}
      >
        <h2 className="text-sm font-semibold text-text-primary mb-2">Remove agent worktree &amp; branch?</h2>
        <p className="text-xs text-text-muted mb-2">
          {pending.nodeName} has {riskText}. Removing also deletes its local branch.
        </p>
        {pending.safety.worktree_path && (
          <p className="text-[11px] font-mono text-text-muted bg-bg-card border border-border-subtle rounded px-2 py-1 mb-5 break-all">
            {pending.safety.worktree_path}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <button
            onClick={() => choose('cancel')}
            className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={() => choose('keep')}
            className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded transition-colors"
          >
            Keep worktree &amp; branch
          </button>
          <button
            onClick={() => choose('remove')}
            className="px-3 py-1.5 text-xs text-white bg-status-error/80 hover:bg-status-error rounded transition-colors"
          >
            Remove anyway
          </button>
        </div>
      </div>
    </div>
  );
}
