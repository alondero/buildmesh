/**
 * `PreferencesCorruptionPanel` — the actionable corruption state for
 * `preferences.json` (issue #1523).
 *
 * Before #1523 a truncated or forward-incompatible `preferences.json`
 * silently became writable defaults: the next ordinary settings change
 * replaced the file, and provider accounts, API keys, pairings, harness
 * defaults, and Autopilot settings were gone for good. The backend now
 * preserves the bytes and refuses writes, which means the *user* has to
 * decide what happens next — so this panel states the situation, says
 * plainly that nothing has been lost yet, and offers the three real
 * options:
 *
 *   1. **Restore the last-known-good backup** — the non-destructive path.
 *      Offered only when the backend says a backup exists *and* parses;
 *      a dead-end button that fails on click is worse than no button.
 *   2. **Open the file location** — for a user who wants to back the file
 *      up or hand-repair it in an editor.
 *   3. **Reset to defaults** — the only destructive option, behind a
 *      `ConfirmDialog` that enumerates what is lost (per DESIGN.md,
 *      destructive confirmations name the loss, they don't say "are you
 *      sure?"). Even this archives the original, and the confirmation says
 *      where it goes.
 *
 * Sits in the modal's shared surface above the panes rather than inside
 * one, because the condition is app-wide: the settings it protects are
 * spread across every pane.
 */
import { useState } from 'react';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { formatError } from '../../lib/errorUtils';
import * as api from '../../lib/tauri';
import { addToast } from '../../stores/toastStore';

/** Why the file could not be read, in the user's terms. The backend sends a
 *  content-free `detail` string too; this maps the machine reason to a
 *  sentence a user can act on ("a newer Buildmesh" is a very different
 *  situation from "the file is empty"). */
function reasonSentence(reason: api.CorruptionReason): string {
  switch (reason) {
    case 'invalid_json':
      return 'The settings file is damaged or was cut short — an interrupted write, a partial copy, or an edit that left it incomplete.';
    case 'not_an_object':
      return 'The settings file does not contain a Buildmesh settings object.';
    case 'schema_mismatch':
      return 'The settings file contains a field this version of Buildmesh cannot read — usually because it was written by a newer version.';
    default:
      return 'The settings file could not be read.';
  }
}

export function PreferencesCorruptionPanel({
  corruption,
  onRecovered,
}: {
  corruption: api.CorruptionInfo;
  /** Re-run the modal's loaders after a successful recovery so every pane
   *  reflects the restored preferences. */
  onRecovered: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmingReset, setConfirmingReset] = useState(false);

  const run = async (action: () => Promise<api.RecoveryOutcome>) => {
    setBusy(true);
    setError(null);
    try {
      const outcome = await action();
      // A toast, not the panel: the panel's job is the *decision*, and it
      // disappears as soon as the file is readable again. The outcome
      // message names the archive the replaced bytes went to, which the user
      // needs to find later and cannot get once this panel is gone.
      addToast('settings', outcome.message, 'success');
      onRecovered();
    } catch (e) {
      // The panel stays up: a failed recovery leaves the file exactly as it
      // was, so the user still needs these actions.
      setError(formatError(e));
    } finally {
      setBusy(false);
      setConfirmingReset(false);
    }
  };

  const reveal = async () => {
    setError(null);
    try {
      await api.openPreferencesLocation();
    } catch (e) {
      setError(formatError(e));
    }
  };

  return (
    <div
      className="mb-4 bg-bg-card border border-status-warning/40 rounded-md px-4 py-3 flex flex-col gap-3"
      // Assertive, like the sibling `ResourceLoadStatus` failure banner:
      // this changes what every control in the modal does, and the user
      // should hear about it the moment Settings opens rather than on
      // their first attempt to save.
      role="alert"
      aria-live="assertive"
      aria-label="Buildmesh settings could not be read"
      data-testid="preferences-corruption"
    >
      <div className="flex flex-col gap-1">
        <span className="font-medium text-text-primary text-base">
          Buildmesh couldn’t read your settings file.
        </span>
        <span className="text-base text-text-secondary">
          {reasonSentence(corruption.reason)} Your file has not been changed and
          Buildmesh will not overwrite it, but settings changes are disabled
          until you choose what to do.
        </span>
        <span className="text-sm text-text-muted">
          {corruption.detail} ({corruption.byte_len.toLocaleString()} bytes at{' '}
          <span className="font-mono break-all">{corruption.path}</span>)
        </span>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        {corruption.backup_available && (
          <button
            type="button"
            disabled={busy}
            onClick={() => run(api.restorePreferencesBackup)}
            className="px-3 py-1 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-sm rounded-md hover:bg-bg-card-hover disabled:opacity-50"
            data-testid="preferences-restore"
            aria-label="Restore the last-known-good settings backup"
          >
            Restore last-known-good
          </button>
        )}
        <button
          type="button"
          disabled={busy}
          onClick={reveal}
          className="px-3 py-1 bg-bg-card border border-border-default font-medium text-text-primary text-sm rounded-md hover:bg-bg-card-hover disabled:opacity-50"
          data-testid="preferences-open-location"
          aria-label="Open the folder containing the settings file"
        >
          Open file location
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={() => setConfirmingReset(true)}
          className="px-3 py-1 bg-bg-card border border-status-error/60 font-medium text-status-error text-sm rounded-md hover:bg-bg-card-hover disabled:opacity-50"
          data-testid="preferences-reset"
          aria-label="Reset settings to defaults"
        >
          Reset to defaults…
        </button>
      </div>

      {corruption.backup_available ? (
        <p className="text-sm text-text-muted">
          Restoring puts back the settings Buildmesh last saved successfully. The
          current file is kept as a copy either way.
        </p>
      ) : (
        <p className="text-sm text-text-muted">
          No saved backup is available to restore, so the options are opening the
          file to repair it by hand, or starting from defaults.
        </p>
      )}

      {error && (
        <p className="text-sm text-status-error" data-testid="preferences-corruption-error">
          {error}
        </p>
      )}

      {confirmingReset && (
        <ConfirmDialog
          title="Reset settings to defaults?"
          // Names the loss rather than asking "are you sure?" — DESIGN.md's
          // rule for destructive confirmations.
          message="Your provider accounts and API keys, provider pairings, harness and model defaults, launch configurations, and Autopilot settings will be replaced with defaults. The unreadable file is kept as a copy, but the settings it held will not come back."
          confirmLabel="Reset settings"
          onConfirm={() => run(api.resetAppPreferences)}
          onCancel={() => setConfirmingReset(false)}
        />
      )}
    </div>
  );
}
