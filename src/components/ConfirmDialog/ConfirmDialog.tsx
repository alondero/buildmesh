import { Modal } from '../shared/Modal';

interface ConfirmDialogProps {
  title: string;
  message: string;
  confirmLabel?: string;
  /**
   * Visual treatment of the confirm button. `'danger'` is the
   * historical default — red, signalling an irreversible or
   * destructive action (delete node, delete mesh property, etc.).
   * `'primary'` renders the cyan accent and is used by flows that
   * are confirmable but not strictly destructive (squash merge,
   * removing a GitHub label that can be re-applied).
   */
  tone?: 'danger' | 'primary';
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({
  title,
  message,
  confirmLabel = 'Delete',
  tone = 'danger',
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const confirmClass =
    tone === 'primary'
      ? 'bg-accent-cyan/80 hover:bg-accent-cyan text-text-inverse'
      : 'bg-status-error/80 hover:bg-status-error text-white';
  return (
    <Modal onClose={onCancel} labelledBy="confirm-dialog-title" maxWidth="max-w-sm">
      <h2 id="confirm-dialog-title" className="text-sm font-semibold text-text-primary mb-2">{title}</h2>
      <p className="text-xs text-text-muted mb-5">{message}</p>
      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={onCancel}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={onConfirm}
          className={`px-3 py-1.5 text-xs rounded-md transition-colors ${confirmClass}`}
        >
          {confirmLabel}
        </button>
      </div>
    </Modal>
  );
}
