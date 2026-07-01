import { Modal } from '../shared/Modal';

interface ConfirmDialogProps {
  title: string;
  message: string;
  confirmLabel?: string;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({ title, message, confirmLabel = 'Delete', onConfirm, onCancel }: ConfirmDialogProps) {
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
          className="px-3 py-1.5 text-xs text-white bg-status-error/80 hover:bg-status-error rounded-md transition-colors"
        >
          {confirmLabel}
        </button>
      </div>
    </Modal>
  );
}
