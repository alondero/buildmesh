// Blocked worktree cleanup dialog (issue #2139).
//
// Closing an Agent Node defers its worktree removal to a durable queue. A
// removal that cannot complete is a *blocked cleanup*: the directory stays, the
// queue row keeps the evidence, and this dialog is the actionable surface.
//
// Each blocked entry shows the node's identity, the full worktree path, which
// removal step failed and the OS error, how many attempts have been made and
// when the next automatic retry may run. The actions are the four recoveries the
// issue asks for:
//
//   * Copy path — the on-disk folder, for Explorer / a terminal.
//   * Copy diagnostics — the whole evidence block, for a bug report.
//   * What is holding it? — a read-only process diagnosis (never automatic).
//   * Retry — attempt the removal now, ignoring the backoff.
//   * Keep worktree — cancel the cleanup intent (nothing on disk changes).
//   * End process — on a diagnosed blocker only, after the user picks it.
//
// The app never closes an application by itself: killing is reachable only from
// a row the diagnosis returned, and the button says which process it would end.

import { useState } from 'react';
import { Modal } from '../shared/Modal';
import { useBlockedCleanupStore } from '../../stores/blockedCleanupStore';
import { formatError } from '../../lib/errorUtils';
import {
  formatCleanupDiagnostics,
  lastAttemptLabel,
  nextRetryLabel,
  operationLabel,
} from '../../lib/worktreeCleanupDiagnostics';
import { addToast } from '../../stores/toastStore';

export function BlockedCleanupDialog() {
  const entries = useBlockedCleanupStore((s) => s.entries);
  const isOpen = useBlockedCleanupStore((s) => s.isOpen);
  const loading = useBlockedCleanupStore((s) => s.loading);
  const blockers = useBlockedCleanupStore((s) => s.blockers);
  const actionStatus = useBlockedCleanupStore((s) => s.actionStatus);
  const close = useBlockedCleanupStore((s) => s.close);
  const retry = useBlockedCleanupStore((s) => s.retry);
  const dismiss = useBlockedCleanupStore((s) => s.dismiss);
  const diagnose = useBlockedCleanupStore((s) => s.diagnose);
  const release = useBlockedCleanupStore((s) => s.release);
  const [nowMs] = useState(() => Date.now());

  if (!isOpen) return null;

  const copy = async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text);
      addToast('Worktree', `${label} copied to the clipboard.`, 'success');
    } catch (error) {
      addToast('Worktree', `Couldn't copy the ${label.toLowerCase()}: ${formatError(error)}`, 'error');
    }
  };

  return (
    <Modal onClose={close} labelledBy="blocked-cleanup-title" maxWidth="max-w-lg">
      <h2 id="blocked-cleanup-title" className="text-sm font-semibold text-text-primary mb-2">
        Worktree cleanup blocked ({entries.length})
      </h2>
      <p className="text-xs text-text-secondary mb-4">
        Closing the node(s) succeeded — these worktree folders could not be removed and stay
        queued. Another app holding a folder open is the usual cause; you can copy what is
        blocked, see what may be holding it, retry now, or keep a worktree in place and stop
        the cleanup.
      </p>

      {loading && entries.length === 0 && (
        <p className="text-xs text-text-muted mb-4">Checking the queue…</p>
      )}

      <div className="max-h-72 overflow-y-auto space-y-3 mb-4">
        {entries.map((entry) => {
          const processes = blockers[entry.worktree_path] ?? [];
          const status = actionStatus[entry.worktree_path];
          return (
            <div
              key={entry.worktree_path}
              className="rounded-md border border-border-subtle bg-bg-card p-3"
              data-testid="blocked-cleanup-entry"
            >
              <div className="text-xs font-semibold text-text-primary">{entry.node_name}</div>
              <p className="text-2xs font-mono text-text-secondary bg-bg-surface border border-border-subtle rounded-md px-2 py-1 mt-1 break-all">
                {entry.worktree_path}
              </p>
              <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-2 gap-y-1 text-xs text-text-secondary">
                <dt className="text-text-muted">Failed at</dt>
                <dd>{operationLabel(entry.last_operation)}</dd>
                <dt className="text-text-muted">Error</dt>
                <dd className="break-words">{entry.last_error ?? 'none recorded'}</dd>
                <dt className="text-text-muted">Attempts</dt>
                <dd>{entry.attempt_count}</dd>
                <dt className="text-text-muted">Last try</dt>
                <dd>{lastAttemptLabel(entry, nowMs)}</dd>
                <dt className="text-text-muted">Next try</dt>
                <dd>{nextRetryLabel(entry, nowMs)}</dd>
              </dl>

              {processes.length > 0 && (
                <ul className="mt-2 space-y-1">
                  {processes.map((process) => (
                    <li
                      key={process.pid}
                      className="flex items-center gap-2 text-xs text-text-secondary"
                    >
                      <span className="flex-1 truncate" title={process.detail}>
                        {process.name ?? 'unknown process'} (pid {process.pid}) —{' '}
                        {process.reason.replace(/-/g, ' ')}
                      </span>
                      {/* Targeted intervention, one process at a time, and only
                          for a row the diagnosis produced. Never automatic. */}
                      <button
                        type="button"
                        onClick={() => void release(entry.worktree_path, process.pid)}
                        className="shrink-0 px-2 py-1 text-2xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
                      >
                        End process
                      </button>
                    </li>
                  ))}
                </ul>
              )}

              <div className="flex flex-wrap gap-2 mt-3">
                <button
                  type="button"
                  onClick={() => void copy(entry.worktree_path, 'Path')}
                  className="px-2 py-1 text-2xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
                >
                  Copy path
                </button>
                <button
                  type="button"
                  onClick={() => void copy(formatCleanupDiagnostics(entry, processes), 'Diagnostics')}
                  className="px-2 py-1 text-2xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
                >
                  Copy diagnostics
                </button>
                <button
                  type="button"
                  onClick={() => void diagnose(entry.worktree_path)}
                  className="px-2 py-1 text-2xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
                >
                  What is holding it?
                </button>
                <button
                  type="button"
                  onClick={() => void retry(entry.worktree_path)}
                  className="px-3 py-1 text-2xs text-white bg-accent-cyan/80 hover:bg-accent-cyan rounded-md transition-colors"
                >
                  Retry
                </button>
                <button
                  type="button"
                  onClick={() => void dismiss(entry.worktree_path)}
                  className="px-3 py-1 text-2xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
                >
                  Keep worktree
                </button>
              </div>

              {status && <p className="mt-2 text-2xs text-text-muted">{status}</p>}
            </div>
          );
        })}
      </div>

      <div className="flex justify-end">
        <button
          type="button"
          onClick={close}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          Close
        </button>
      </div>
    </Modal>
  );
}
