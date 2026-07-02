import { Modal } from '../shared/Modal';
import { useWorktreeClosePromptStore } from '../../stores/worktreeClosePromptStore';

export function WorktreeCloseDialog() {
  const pending = useWorktreeClosePromptStore(state => state.pending);
  const choose = useWorktreeClosePromptStore(state => state.choose);

  // Escape-to-cancel comes from <Modal> (issue #643: Esc is the dismiss path
  // that works even when the WebView is occluded and backdrop clicks never
  // arrive). `<WorktreeCloseDialog />` is mounted unconditionally in App.tsx,
  // but Modal — and therefore its window keydown listener — only mounts while
  // `pending` is set, so Escape is never stolen from agent CLIs in the grid.
  if (!pending) return null;

  const riskParts = [
    pending.safety.has_uncommitted ? 'uncommitted changes' : null,
    pending.safety.has_unpushed ? 'unpushed or unmerged commits' : null,
  ].filter(Boolean);

  const riskText = riskParts.length > 0 ? riskParts.join(' and ') : 'work that may not be recoverable';

  return (
    <Modal onClose={() => choose('cancel')} labelledBy="worktree-close-title" maxWidth="max-w-md">
      <h2 id="worktree-close-title" className="text-sm font-semibold text-text-primary mb-2">Remove agent worktree &amp; branch?</h2>
      <p className="text-xs text-text-muted mb-2">
        {pending.nodeName} has {riskText}. Removing also deletes its local branch.
      </p>
      {pending.safety.worktree_path && (
        <p className="text-xs font-mono text-text-muted bg-bg-card border border-border-subtle rounded-md px-2 py-1 mb-5 break-all">
          {pending.safety.worktree_path}
        </p>
      )}
      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={() => choose('cancel')}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={() => choose('keep')}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          Keep worktree &amp; branch
        </button>
        <button
          type="button"
          onClick={() => choose('remove')}
          className="px-3 py-1.5 text-xs text-white bg-status-error/80 hover:bg-status-error rounded-md transition-colors"
        >
          Remove anyway
        </button>
      </div>
    </Modal>
  );
}
