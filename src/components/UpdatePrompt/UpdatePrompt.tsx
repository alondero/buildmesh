import { Modal } from '../shared/Modal';
import { useUpdateCheck } from '../../hooks/useUpdateCheck';
import { describeUpdate } from '../../lib/updater';

// In-app "update available" dialog (issue #826). Mounted once at the App root;
// renders nothing until `useUpdateCheck` finds a pending update, so the shared
// Modal (and its Escape/focus-trap machinery) only arms when there's actually
// something to install — mirrors WorktreeCloseDialog's null-until-pending shape.
export function UpdatePrompt() {
  const { available, update, installing, install, dismiss } = useUpdateCheck();

  if (!available || !update) return null;

  const { version, notes, message } = describeUpdate(update);

  return (
    <Modal onClose={dismiss} labelledBy="update-prompt-title" maxWidth="max-w-sm">
      <h2 id="update-prompt-title" className="text-sm font-semibold text-text-primary mb-2">
        Update available
      </h2>
      <p className="text-xs text-text-muted mb-3">{message}</p>
      {notes && (
        <pre className="text-xs text-text-secondary bg-bg-base/60 border border-border-subtle rounded-md p-2 mb-5 max-h-40 overflow-auto whitespace-pre-wrap">
          {notes}
        </pre>
      )}
      <div className="flex justify-end gap-2">
        <button
          type="button"
          onClick={dismiss}
          disabled={installing}
          className="px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary border border-border-subtle rounded-md transition-colors disabled:opacity-50"
        >
          Later
        </button>
        <button
          type="button"
          onClick={install}
          disabled={installing}
          className="px-3 py-1.5 text-xs font-medium rounded-md bg-accent-cyan/10 text-accent-cyan border border-accent-cyan/20 hover:bg-accent-cyan/20 transition-colors disabled:opacity-50"
        >
          {installing ? `Installing v${version}…` : 'Install & Restart'}
        </button>
      </div>
    </Modal>
  );
}
