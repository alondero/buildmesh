/**
 * `AccountCard` — one provider account's editable card (enabled toggle,
 * API key, billing mode). Extracted from `AppSettingsModal` in issue
 * #1880 so the Providers pane owns its own credential UI; the modal now
 * holds only tab chrome and dirty tracking.
 *
 * The card owns its dirty reporting: it fires `onDirtyChange(true)` when
 * an uncommitted edit exists and `false` when it returns to pristine AND
 * on unmount, so the modal's discard banner never leaks a stale site.
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { formatError } from '../../lib/errorUtils';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { isSelfAuthId, isFirstClassId } from '../../lib/providerClassification';
import { normalizeApiKey } from './settingsUtils';
import { isWindows } from '../../lib/platform';
import type { ProviderAccount } from '../../lib/tauri';

/** Per-field editable overlays. Each field is independently tracked:
 *  - key absent from the object → user hasn't touched this field; the
 *    card renders the value from `account`.
 *  - key present → the user's override for that field. `null` for
 *    `api_key` means "user explicitly cleared the key"; the absence of
 *    the key means "field is pristine, follow account".
 *
 *  Per-field tracking (not a monolithic snapshot) is what fixes the
 *  field-shadowing bug from the round-3 review (Finding 2): editing only
 *  `api_key` no longer freezes a snapshot of `billing_mode`, so a
 *  concurrent server-side billing change rides through to the IPC payload
 *  instead of being clobbered by the user's stale snapshot. */
export type EditOverrides = {
  api_key?: string | null;
  billing_mode?: ProviderAccount['billing_mode'];
};

export function AccountCard({
  account,
  onSave,
  onRemove,
  onDirtyChange,
  disabled = false,
  keyInPreferences = false,
}: {
  account: ProviderAccount;
  onSave: (account: ProviderAccount) => Promise<boolean>;
  onRemove?: (id: string) => Promise<void>;
  /**
   * Fires with `true` when the card has uncommitted edits, `false` when
   * it returns to pristine, AND `false` on unmount so the modal's discard
   * banner doesn't leak a stale dirty site. The parent aggregates these
   * signals across the modal so a stray backdrop click can prompt before
   * destroying half-typed credentials (issue #730). The unmount cleanup
   * is a documented contract — the round-3 review flagged that omitting
   * it trapped users with the discard banner after removing a dirty card.
   */
  onDirtyChange?: (dirty: boolean) => void;
  /** Issue #1523 — set while `preferences.json` exists but cannot be read.
   *  Every control here writes `provider_accounts`, which the backend
   *  refuses while the file is corrupt, so a live card would let the user
   *  type a credential only to have the save rejected. Same contract as
   *  `HarnessDefaultsSection` / `ProbeSpawnPromptsSection`. */
  disabled?: boolean;
  /** Issue #2154 — this account's key is persisted in `preferences.json`
   *  because the OS credential store could not be used. Renders a notice
   *  saying so, on the account that is affected. Defaults false so the
   *  common case (every key safely in the credential store) shows nothing
   *  and no caller has to prove the negative. */
  keyInPreferences?: boolean;
}) {
  // Issue #1535 (round 4, PR #1636 review round 3): per-field overrides
  // replace the round-3 nullable snapshot. isDirty is derived purely from
  // `Object.keys(overrides).length > 0` — no ref, no imperative flag, no
  // nullable-snapshot distinction to manage. The display reads the
  // override if present, otherwise `account.*` directly. The toggle does
  // not touch overrides, so it cannot erase typed edits (Finding 1).
  const [overrides, setOverrides] = useState<EditOverrides>({});
  const [showCreds, setShowCreds] = useState(false);
  // Issue #1523: folding the parent's `disabled` into `busy` keeps every
  // existing per-control `disabled={busy}` correct without adding a prop to
  // each of them — a locked card simply has no live control.
  const [saving, setBusy] = useState(false);
  const busy = saving || disabled;
  const [confirmingRemove, setConfirmingRemove] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Baseline = the latest `account` prop's editable values, normalised
  // (empty-string api_key collapses to null). Display values fall through
  // the override first, then the baseline. The fall-through chain means
  // prop changes are reflected automatically with no useEffect — the bug
  // round-1/2/3 all danced around is gone by construction.
  const baselineApiKey = normalizeApiKey(account.api_key);
  const baselineBillingMode = account.billing_mode;
  const apiKeyDisplay =
    overrides.api_key !== undefined ? overrides.api_key : baselineApiKey;
  const billingDisplay =
    overrides.billing_mode !== undefined
      ? overrides.billing_mode
      : baselineBillingMode;
  // Pure derivation: isDirty is the count of touched fields. No ref, no
  // imperative flag.
  const isDirty = Object.keys(overrides).length > 0;

  // Per-field setters that remove the override when the user's edit
  // matches the baseline (round-3 Finding 3 — backspacing to the
  // original value automatically reverts). Editing one field never
  // touches the other's override (round-4 Finding 2 — field-shadowing
  // fix).
  const editApiKey = useCallback(
    (rawValue: string) => {
      const next = normalizeApiKey(rawValue);
      setOverrides(prev => {
        if (next === baselineApiKey) {
          // Revert: drop the api_key override. Preserve other overrides.
          const { api_key: _dropped, ...rest } = prev;
          return rest;
        }
        return { ...prev, api_key: next };
      });
      setError(null);
    },
    [baselineApiKey],
  );

  const editBillingMode = useCallback(
    (value: ProviderAccount['billing_mode']) => {
      setOverrides(prev => {
        if (value === baselineBillingMode) {
          const { billing_mode: _dropped, ...rest } = prev;
          return rest;
        }
        return { ...prev, billing_mode: value };
      });
      setError(null);
    },
    [baselineBillingMode],
  );

  // account.id drives the confirmingRemove reset on row identity change
  // (the account was removed and re-added with a different id).
  useEffect(() => {
    setConfirmingRemove(false);
  }, [account.id]);

  // Dirty notification (issue #730). Compare to `lastReportedDirtyRef` so
  // we don't re-fire the parent's `setDirtySites` for every keystroke.
  const lastReportedDirtyRef = useRef<boolean>(false);
  useEffect(() => {
    if (lastReportedDirtyRef.current === isDirty) return;
    onDirtyChange?.(isDirty);
    lastReportedDirtyRef.current = isDirty;
  }, [isDirty, onDirtyChange]);

  // Unmount cleanup: per the JSDoc on `onDirtyChange` (and issue #730's
  // contract), fire `false` when the card unmounts so the modal's
  // `dirtySites` set doesn't retain a stale site. Without this, removing
  // a dirty card (or any unmount path) traps the user with the discard
  // banner forever.
  useEffect(() => {
    return () => {
      onDirtyChange?.(false);
    };
    // The initial onDirtyChange closes over this card's account.id; the
    // parent uses stable useCallback chains, so the captured reference
    // is still valid at unmount time even though we don't list it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const removable = !isSelfAuthId(account.id);
  // Keyed providers show API key; self-auth hold no Buildmesh credentials.
  const showApiKey = account.claude_compatible;
  // Billing mode is first-class only (self-auth + keyed catalog).
  const showBilling = isFirstClassId(account.id);

  // Toggle commits ONLY the enabled flag. The form draft is never touched
  // — the typed API key (if any) survives the parent re-render that
  // follows the IPC, because nothing in the toggle path sets `draft`. On
  // failure the parent's existing top-level toast handles reporting; we
  // do NOT set a card-local error because the inline alert's Retry button
  // is wired to `saveDraft` (Finding 4). A toggle retry would need its
  // own affordance — out of scope for this issue.
  const toggleEnabled = async (enabled: boolean) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await onSave({ ...account, enabled });
    } finally {
      setBusy(false);
    }
  };

  // Save composes the IPC payload from the latest `account` prop + the
  // user's overrides. Server-side renames (`account.name`) ride along
  // via the spread. Fields the user DIDN'T touch come from `account.*`
  // — that's the field-shadowing fix (round-4 Finding 2): a server-side
  // billing change is never clobbered by a stale snapshot from when the
  // user started editing.
  const saveDraft = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const ok = await onSave({
        ...account,
        api_key: apiKeyDisplay,
        billing_mode: billingDisplay,
      });
      if (ok) {
        setOverrides({});
      } else {
        // Inline alert is the navigation affordance for the Retry button.
        // The backend's actual error already surfaces in the parent's top-
        // level toast, so we deliberately don't echo it here.
        setError('Save failed');
      }
    } catch (e) {
      setError(formatError(e));
    } finally {
      setBusy(false);
    }
  };

  const confirmRemove = async () => {
    if (busy) return;
    setBusy(true);
    setConfirmingRemove(false);
    try {
      await onRemove?.(account.id);
    } catch {
      // Parent shows the error toast.
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="border border-border-subtle rounded-lg p-5">
      <div className="flex items-center gap-3 mb-3">
        <ProviderIcon providerId={account.id} className="h-6 w-6" />
        <span className="text-lg font-medium text-text-primary">{account.name}</span>
        <div className="ml-auto flex items-center gap-3">
          <label className="flex items-center gap-2 text-base text-text-secondary cursor-pointer">
            <input
              type="checkbox"
              checked={account.enabled}
              disabled={busy}
              onChange={e => toggleEnabled(e.target.checked)}
              className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
              aria-label={`Enable ${account.name}`}
            />
            <span>Enabled</span>
          </label>
          {removable && onRemove && (confirmingRemove ? (
            <div className="flex items-center gap-2">
              <span className="text-sm text-status-error">Remove {account.name}?</span>
              <button
                onClick={confirmRemove}
                disabled={busy}
                className="px-3 py-1 bg-status-error text-white text-sm rounded-md hover:bg-status-error/90 disabled:opacity-50"
                aria-label={`Confirm remove ${account.name}`}
              >
                Yes
              </button>
              <button
                onClick={() => setConfirmingRemove(false)}
                disabled={busy}
                className="px-3 py-1 bg-bg-card text-text-secondary text-sm rounded-md hover:bg-bg-card/70 disabled:opacity-50"
                aria-label={`Cancel remove ${account.name}`}
              >
                No
              </button>
            </div>
          ) : (
            <button
              onClick={() => setConfirmingRemove(true)}
              disabled={busy}
              className="px-3 py-1 bg-status-error/15 text-status-error text-sm rounded-md hover:bg-status-error/25 disabled:opacity-50"
              aria-label={`Remove ${account.name}`}
              title={`Remove ${account.name}`}
            >
              Remove
            </button>
          ))}
        </div>
      </div>

      {/* Issue #2154 — where this account's key actually lives. Sits outside
          the "Edit credentials" disclosure on purpose: the person who needs
          this is the one who has NOT opened the credential editor, and the
          whole point of the issue is that the fallback used to be invisible.
          Polite rather than assertive — it is a standing condition about
          storage, not a failure that just happened. */}
      {keyInPreferences && (
        <div
          role="status"
          aria-live="polite"
          data-testid={`account-key-in-preferences-${account.id}`}
          className="mt-3 flex items-start gap-2 rounded-md border border-status-warning/40 bg-status-warning/10 px-3 py-2 text-sm text-text-secondary"
        >
          <span className="text-status-warning" aria-hidden="true">⚠</span>
          <span>
            <span className="font-medium text-text-primary">
              This {account.name} API key is stored in the preferences.json file.
            </span>{' '}
            {isWindows
              ? 'Buildmesh keeps provider keys in the Windows Credential Manager, but it could not reach it, so this key stayed on disk in your app data folder in plain text. Sign in to Windows and save the key again to move it into the credential store.'
              : 'Buildmesh keeps provider keys in the OS credential store, which this platform does not provide, so this key stays in your app data folder in plain text.'}
          </span>
        </div>
      )}

      {(showApiKey || showBilling) && (
        <button
          onClick={() => setShowCreds(v => !v)}
          className="mt-3 text-sm text-text-secondary hover:text-text-primary"
        >
          {showCreds ? 'Hide settings' : (showApiKey ? 'Edit credentials' : 'Edit billing')}
        </button>
      )}

      {showCreds && (showApiKey || showBilling) && (
        <div className="mt-3 space-y-3">
          {showApiKey && (
            <div>
              <label className="block text-sm text-text-muted mb-1">API key</label>
              <input
                type="password"
                value={apiKeyDisplay ?? ''}
                disabled={busy}
                onChange={e => editApiKey(e.target.value)}
                placeholder="Enter API key..."
                className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
                aria-label={`${account.name} API key`}
              />
              <p className="mt-1 text-sm text-text-muted">
                Base URL and model names are set when you attach this provider under Harnesses.
              </p>
            </div>
          )}
          {showBilling && (
            <div>
              <label className="block text-sm text-text-muted mb-1">Billing</label>
              <select
                value={billingDisplay}
                disabled={busy}
                onChange={e => editBillingMode(e.target.value as ProviderAccount['billing_mode'])}
                className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
                aria-label={`${account.name} billing mode`}
              >
                <option value="plan">Plan / subscription (percentage)</option>
                <option value="pay_as_you_go">Pay-as-you-go (balance)</option>
              </select>
            </div>
          )}
          <div className="flex gap-3 items-center">
            <button
              onClick={saveDraft}
              disabled={busy}
              className="px-5 py-2 bg-bg-selection border border-accent-cyan font-medium text-text-primary text-base rounded-md hover:bg-bg-card-hover disabled:opacity-50"
            >
              {saving ? 'Saving...' : 'Save'}
            </button>
          </div>
          {error && (
            <div
              role="alert"
              data-testid="account-card-error"
              className="flex items-center gap-2 text-sm text-status-error"
            >
              <span>{error}</span>
              <button
                type="button"
                onClick={saveDraft}
                disabled={busy}
                className="underline hover:no-underline disabled:opacity-50"
              >
                Retry
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}