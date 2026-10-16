import { Modal } from '../shared/Modal';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { useWorktreeClosePromptStore } from '../../stores/worktreeClosePromptStore';
import { useChangedFiles } from '../../hooks/useChangedFiles';
import { fileDiffStatusMeta } from '../../lib/status';
import { ChangedFileRow } from '../FileTree/ChangedFileRow';

export function WorktreeCloseDialog() {
  const pending = useWorktreeClosePromptStore(state => state.pending);
  const choose = useWorktreeClosePromptStore(state => state.choose);

  // Fetch the uncommitted file list so the user can see exactly what "Remove
  // anyway" would discard. Keyed on the worktree path via the shared
  // `gitStatusClient`, so it dedupes with any panel already showing this repo
  // and refreshes on GIT_CHANGED. Passing `null` (no prompt, or an
  // unpushed-only risk) disables the hook — it must be called unconditionally
  // here, before the early return below, to satisfy the rules of hooks.
  //
  // Issue #2328: `files` is `null` until a fetch succeeds, so the dialog can
  // now tell "the status is unavailable" apart from "git reports nothing
  // uncommitted". Neither branch is allowed to imply the worktree is safe —
  // the risk sentence and the buttons below come from the backend's
  // `pending.safety` snapshot and are deliberately left untouched.
  const worktreePath = pending?.safety.worktree_path ?? null;
  const showFiles = Boolean(pending?.safety.has_uncommitted && worktreePath);
  const { files, loading, stale } = useChangedFiles(showFiles ? worktreePath : null);

  // Escape-to-cancel comes from <Modal> (issue #643: Esc is the dismiss path
  // that works even when the WebView is occluded and backdrop clicks never
  // arrive). `<WorktreeCloseDialog />` is mounted unconditionally in App.tsx,
  // but Modal — and therefore its window keydown listener — only mounts while
  // `pending` is set, so Escape is never stolen from agent CLIs in the grid.
  if (!pending) return null;

  if (pending.reason === 'active-session') {
    return <ConfirmDialog title="Close active session?"
      message={`${pending.nodeName} is still active. Closing stops its process and removes the node. Files in the repository will be kept.`}
      confirmLabel="Close session" onConfirm={() => choose('remove')} onCancel={() => choose('cancel')} />;
  }

  const riskParts = [
    pending.safety.has_uncommitted ? 'uncommitted changes' : null,
    pending.safety.has_unpushed ? 'unpushed or unmerged commits' : null,
  ].filter(Boolean);

  const riskText = riskParts.length > 0 ? riskParts.join(' and ') : 'work that may not be recoverable';

  return (
<Modal onClose={() => choose('cancel')} labelledBy="worktree-close-title" maxWidth="max-w-md">
      <h2 id="worktree-close-title" className="text-sm font-semibold text-text-primary mb-2">Remove agent worktree &amp; branch?</h2>
      <p className="text-xs text-text-secondary mb-2">
        {pending.nodeName} has {riskText}. Removing also deletes its local branch.
      </p>
      {pending.safety.worktree_path && (
        <p className={`text-xs font-mono text-text-secondary bg-bg-card border border-border-subtle rounded-md px-2 py-1 break-all ${pending.safety.has_uncommitted ? 'mb-2' : 'mb-5'}`}>
          {pending.safety.worktree_path}
        </p>
      )}
      {pending.safety.has_uncommitted && (
        <div className="mb-5">
          <div className="text-2xs uppercase tracking-wide text-text-muted mb-1">
            Uncommitted files{files !== null && files.length > 0 ? ` (${files.length})` : ''}
            {stale && <span className="normal-case tracking-normal"> · last known</span>}
          </div>
          <div className="max-h-40 overflow-y-auto rounded-md border border-border-subtle bg-bg-card">
            {loading && files === null ? (
              <div className="px-2 py-1 text-xs text-text-muted">Loading…</div>
            ) : files === null ? (
              // No successful observation for this path: say the listing is
              // unavailable rather than implying the worktree is clean.
              <div className="px-2 py-1 text-xs text-text-muted">Couldn&apos;t list the changed files.</div>
            ) : files.length === 0 ? (
              // Git itself reported nothing uncommitted. Scoped to this
              // moment on purpose: it must not read as "nothing is at risk",
              // because the risk sentence above still reflects the backend's
              // safety snapshot and the user still has to confirm.
              <div className="px-2 py-1 text-xs text-text-muted">Git reports no uncommitted files right now.</div>
            ) : (
              files.map((file) => {
                const meta = fileDiffStatusMeta(file.status);
                return (
                  <div
                    key={file.path}
                    title={file.path}
                    className="flex items-center gap-2 px-2 py-0.5 text-xs font-mono"
                  >
                    <ChangedFileRow file={file} meta={meta} />
                  </div>
                );
              })
            )}
          </div>
        </div>
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
