import { formatError } from '../../lib/errorUtils';
import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { listen } from '@tauri-apps/api/event';
import { ProviderIcon } from '../Providers/ProviderIcon';
import { HarnessOrderList } from './HarnessOrderList';
import { OpenCodeAccountCard } from './OpenCodeAccountCard';
import { HarnessConfigList, type ProxyHarness } from './HarnessConfigList';
import { HarnessDefaultsSection } from './HarnessDefaultsSection';
import { UpdateAboutSection } from './UpdateAboutSection';
import * as api from '../../lib/tauri';
import type {
  ProviderInfo,
  ProviderAccount,
  ProviderPairing,
  PairingVerification,
  DeviceSession,
  RealizedBind,
} from '../../lib/tauri';
import type { HarnessConfigValue } from '../../types/generated/HarnessConfigValue';
import { optimisticToggle } from '../../lib/optimisticToggle';
import { useExitPromptStore } from '../../stores/exitPromptStore';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { currentTheme, setTheme, type ThemeName } from '../../lib/theme';
import { isSelfAuthId, isFirstClassId, KEYED_FIRST_CLASS_IDS } from '../../lib/providerClassification';
import { isWindows } from '../../lib/platform';
import { useSettingsResources, type ResourceKey, type ResourceState } from './useSettingsResources';

interface AppSettingsModalProps {
  onClose: () => void;
}

const NO_OVERRIDE = '__no_override__';

async function getHostPairingVerifications(): Promise<PairingVerification[]> {
  const runtimes: api.EnvType[] = isWindows ? ['windows', 'wsl'] : ['windows'];
  return (await Promise.all(runtimes.map((runtime) => api.getPairingVerifications(runtime)))).flat();
}

/** The Settings sub-panes. One long scroll of unrelated sections outgrew
 *  itself; each pane groups settings by concern (behaviour defaults /
 *  provider credentials / spawn-menu composition / network reachability).
 *  All panes stay MOUNTED (inactive ones get the `hidden` attribute) — the
 *  modal's dirty tracking (issue #730) lives in child component state, so
 *  unmounting a pane on tab-switch would destroy half-typed credentials
 *  while the modal still reports itself dirty. */
const SETTINGS_TABS = [
  { id: 'general', label: 'General' },
  { id: 'providers', label: 'Providers' },
  { id: 'harnesses', label: 'Harnesses' },
  { id: 'remote', label: 'Remote Access' },
] as const;
type SettingsTabId = (typeof SETTINGS_TABS)[number]['id'];

/** Which pane a dirty site belongs to, so its nav item can show the
 *  unsaved-changes dot. Site keys: `autopilot-pool` + `harness-defaults`
 *  (General), `harness-*` (Harnesses, prefixed where the modal wires
 *  HarnessConfigList), and `account-*` / `add-custom-form` (Providers). */
function paneForDirtySite(site: string): SettingsTabId {
  if (site === 'autopilot-pool' || site === 'harness-defaults' || site === 'worktree-dir') return 'general';
  if (site.startsWith('harness-')) return 'harnesses';
  return 'providers';
}

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
type EditOverrides = {
  api_key?: string | null;
  billing_mode?: ProviderAccount['billing_mode'];
};

/** Normalise empty-string api_key to null on the way in. The Rust side
 *  sometimes serialises empty keys as `""` rather than `null`; treating
 *  those as the same value lets the smart collapse actually collapse
 *  when the user backspaces to empty. */
function normalizeApiKey(value: string | null | undefined): string | null {
  return value ? value : null;
}

export function AccountCard({
  account,
  onSave,
  onRemove,
  onDirtyChange,
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
}) {
  // Issue #1535 (round 4, PR #1636 review round 3): per-field overrides
  // replace the round-3 nullable snapshot. isDirty is derived purely from
  // `Object.keys(overrides).length > 0` — no ref, no imperative flag, no
  // nullable-snapshot distinction to manage. The display reads the
  // override if present, otherwise `account.*` directly. The toggle does
  // not touch overrides, so it cannot erase typed edits (Finding 1).
  const [overrides, setOverrides] = useState<EditOverrides>({});
  const [showCreds, setShowCreds] = useState(false);
  const [busy, setBusy] = useState(false);
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
              className="px-5 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50"
            >
              {busy ? 'Saving...' : 'Save'}
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

/** Add a keyed first-class provider from the catalog, or a generic (name + key). */
export function AddProviderForm({
  catalog,
  onAddCatalog,
  onAddGeneric,
  onCancel,
  onDirtyChange,
}: {
  catalog: ProviderAccount[];
  onAddCatalog: (template: ProviderAccount) => Promise<void>;
  onAddGeneric: (name: string, apiKey: string) => Promise<void>;
  onCancel: () => void;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const [mode, setMode] = useState<'pick' | 'generic'>('pick');
  const [name, setName] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [busy, setBusy] = useState(false);

  const isDirty =
    mode === 'generic'
      ? name.trim() !== '' || apiKey.trim() !== ''
      : false;
  const lastReportedDirtyRef = useRef<boolean>(false);
  useEffect(() => {
    if (lastReportedDirtyRef.current === isDirty) return;
    onDirtyChange?.(isDirty);
    lastReportedDirtyRef.current = isDirty;
  }, [isDirty, onDirtyChange]);

  const addCatalog = async (template: ProviderAccount) => {
    setBusy(true);
    try {
      await onAddCatalog(template);
    } finally {
      setBusy(false);
    }
  };

  const submitGeneric = async () => {
    setBusy(true);
    try {
      await onAddGeneric(name, apiKey);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-4 border border-border-subtle rounded-lg p-5 space-y-3" data-testid="add-provider-form">
      {mode === 'pick' ? (
        <>
          <p className="text-base text-text-secondary">
            Add a first-class provider, or configure any other provider with a name and API key.
            Endpoint URL and models are set when you attach it under Harnesses.
          </p>
          {catalog.length > 0 && (
            <ul className="space-y-2">
              {catalog.map((t) => (
                <li key={t.id}>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => addCatalog(t)}
                    className="w-full flex items-center gap-3 border border-border-subtle rounded-md px-4 py-3 text-left hover:border-accent-cyan disabled:opacity-50"
                    aria-label={`Add ${t.name}`}
                  >
                    <ProviderIcon providerId={t.id} className="h-5 w-5" />
                    <span className="text-base text-text-primary">{t.name}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            disabled={busy}
            onClick={() => setMode('generic')}
            className="text-base text-text-secondary hover:text-text-primary disabled:opacity-50"
          >
            Other / custom…
          </button>
          <div>
            <button
              onClick={onCancel}
              disabled={busy}
              className="px-5 py-2 text-base text-text-muted hover:text-text-secondary disabled:opacity-50"
            >
              Cancel
            </button>
          </div>
        </>
      ) : (
        <>
          <p className="text-base text-text-secondary">
            Generic provider — name and API key only. Attach under Harnesses to set base URL
            (and Claude model tiers when using Claude Code).
          </p>
          <input
            type="text"
            value={name}
            onChange={e => setName(e.target.value)}
            placeholder="Name (e.g. DeepSeek)"
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
            aria-label="Custom provider name"
          />
          <input
            type="password"
            value={apiKey}
            onChange={e => setApiKey(e.target.value)}
            placeholder="API key"
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2 text-base text-text-primary focus:outline-none focus:border-accent-cyan"
            aria-label="Custom provider API key"
          />
          <div className="flex gap-3">
            <button
              onClick={submitGeneric}
              disabled={busy || !name.trim() || !apiKey.trim()}
              className="px-5 py-2 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50"
            >
              {busy ? 'Adding...' : 'Add provider'}
            </button>
            <button
              onClick={() => {
                setMode('pick');
                setName('');
                setApiKey('');
              }}
              disabled={busy}
              className="px-5 py-2 text-base text-text-muted hover:text-text-secondary disabled:opacity-50"
            >
              Back
            </button>
            <button
              onClick={onCancel}
              disabled={busy}
              className="px-5 py-2 text-base text-text-muted hover:text-text-secondary disabled:opacity-50"
            >
              Cancel
            </button>
          </div>
        </>
      )}
    </div>
  );
}

/** Per-resource load status surface (issue #1534). Renders a
 *  loading message while in flight, and on failure an accessible
 *  error banner with a Retry button that rehydrates ONLY this
 *  resource (siblings stay where they are). Replaces the previous
 *  shape where the global `loaded` flag flipped to `true` even after
 *  a failure, leaving the user staring at placeholder default state
 *  with no signal that anything went wrong.
 *
 *  Test-IDs follow the `resource-load-<key>` / `resource-load-<key>-retry`
 *  convention so tests can target a specific resource's banner without
 *  relying on prose that may change. */
function ResourceLoadStatus({
  resource,
  state,
  onRetry,
}: {
  resource: ResourceKey;
  state: ResourceState;
  /** Required for the `failed` banner (renders the Retry button);
   *  unused by the `loading` branch. If `onRetry` is undefined on
   *  a failed banner, we render an alert WITHOUT a Retry button
   *  rather than a button that no-ops — round-5 review caught the
   *  earlier "render the button unconditionally" hazard. */
  onRetry?: () => void;
}) {
  if (state.status === 'failed') {
    return (
      <div
        className="flex items-start gap-2 bg-bg-card border border-status-warning/40 rounded-md px-3 py-2"
        data-testid={`resource-load-${resource}`}
        // Issue #1534 (review round 2) — the failure banner is an
        // alert: screen readers announce it as soon as it appears
        // (`role="alert"` implicitly maps to `aria-live="assertive"`)
        // and a descriptive label gives the message a name beyond
        // "alert". Without these, a user relying on assistive tech
        // would never learn that the resource failed to load.
        role="alert"
        aria-live="assertive"
        aria-label={`Couldn't load ${humanResourceName(resource)}`}
      >
        <span className="flex-1 text-base text-text-primary">
          <span className="font-medium">Couldn’t load {humanResourceName(resource)}.</span>{' '}
          <span className="text-text-muted">{state.error ?? 'Unknown error.'}</span>
        </span>
        {onRetry && (
          <button
            type="button"
            onClick={onRetry}
            className="px-3 py-1 bg-accent-cyan/20 text-accent-cyan text-sm rounded-md hover:bg-accent-cyan/30"
            data-testid={`resource-load-${resource}-retry`}
            // Scoped aria-label so a screen reader user hears which
            // resource this Retry button belongs to — multiple banners
            // on the same pane (e.g. preferences + providers both
          // failing) would otherwise announce two indistinguishable
          // "Retry" buttons.
          aria-label={`Retry loading ${humanResourceName(resource)}`}
        >
          Retry
        </button>
        )}
      </div>
    );
  }
  // loading (default branch — `idle` shouldn't reach the UI yet because
  // the dependent resource hasn't fired; the only resource that starts
  // at `idle` is pairings, which is rendered conditionally below).
  return (
    <p
      className="text-base text-text-muted"
      data-testid={`resource-load-${resource}-loading`}
      // Polite live region so a screen reader announces the
      // "Loading…" state change without interrupting whatever's
      // mid-utterance. `role="status"` carries the implicit
      // `aria-live="polite"`.
      role="status"
      aria-live="polite"
    >
      Loading {humanResourceName(resource)}…
    </p>
  );
}

/** Friendly pane name for a resource (issue #1534). Used in the
 *  per-resource banner so the message reads naturally ("Couldn't load
 *  paired devices" rather than "Couldn't load devices"). Kept
 *  inline-only — it's not a domain term the rest of the codebase
 *  needs. */
function humanResourceName(resource: ResourceKey): string {
  switch (resource) {
    case 'preferences':
      return 'preferences';
    case 'providers':
      return 'providers';
    case 'accounts':
      return 'provider accounts';
    case 'pairings':
      return 'harness pairings';
    case 'coordinator':
      return 'coordinator status';
    case 'devices':
      return 'paired devices';
    case 'network':
      return 'network status';
  }
}

export function AppSettingsModal({ onClose }: AppSettingsModalProps) {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [selected, setSelected] = useState<string>(NO_OVERRIDE);
  const [reviewerProvider, setReviewerProvider] = useState<string>(NO_OVERRIDE);
  const [saving, setSaving] = useState(false);
  const [reviewerSaving, setReviewerSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Issue #1534 — replace the global `loaded: boolean` with per-resource
  // status. The previous flag was set true even on full failure (the
  // catch block still called `setLoaded(true)`), which let placeholder
  // initial state render as authoritative and gave the user writable
  // controls backed by no real data. Each control now refuses to act
  // until its backing resource has loaded; failed resources render an
  // accessible error + Retry and never claim "zero accounts / no
  // compatible providers / no paired devices" when the real answer is
  // "we couldn't read the answer".
  //
  // Review feedback round 2: collapsed seven `useState<ResourceState>`
  // hooks into one `Record<ResourceKey, ResourceState>` so adding a new
  // resource is type-checked (omitting the key from `INITIAL_RESOURCES`
  // Issue #1534 (review round 5) — the resource state machine is
  // now owned by `useSettingsResources`. The hook owns the
  // per-resource status + error + request-id tokens, and exposes
  // loaders + a `retryResource` handler. The modal retains its
  // derived state (providers list, accounts, pairings, verifications,
  // etc.) and wires `onXLoaded` callbacks into the hook so the
  // loaders commit data through the modal's setters.
  const {
    resources,
    loadPreferences,
    loadProviders,
    loadAccounts,
    loadPairings,
    loadCoordinator,
    loadDevices,
    loadNetwork,
    retryResource,
  } = useSettingsResources({
    onPreferencesLoaded: (prefs) => {
      const stored = prefs.default_provider;
      setSelected(stored && stored.length > 0 ? stored : NO_OVERRIDE);
      const storedReviewer = prefs.reviewer_provider;
      setReviewerProvider(storedReviewer && storedReviewer.length > 0 ? storedReviewer : NO_OVERRIDE);
      const storedNaming = prefs.naming_provider;
      setNamingProvider(storedNaming && storedNaming.length > 0 ? storedNaming : null);
      const storedPool = prefs.autopilot_pool_size == null ? '' : String(prefs.autopilot_pool_size);
      setPoolDraft(storedPool);
      poolSavedRef.current = storedPool;
      const storedWorktreeDir = prefs.worktree_directory?.trim() ?? '';
      setWorktreeDirDraft(storedWorktreeDir);
      worktreeDirSavedRef.current = storedWorktreeDir;
      useExitPromptStore.getState().setConfirmBeforeQuit(prefs.confirm_before_quit ?? true);
      setHarnessDefaults(prefs.harness_defaults ?? {});
    },
    onProvidersLoaded: (list) => {
      setProviders(list);
      providersRef.current = list;
    },
    onAccountsLoaded: ({ accountList, catalog }) => {
      setAccounts(accountList);
      setKeyedCatalog(Array.isArray(catalog) ? catalog : []);
    },
    onPairingsLoaded: ({ effective, verifications, storedKeys, compatible }) => {
      setPairings(effective);
      setPairingVerifications(verifications);
      setStoredPairingKeys(storedKeys);
      setCompatibleByHarness(compatible);
    },
    onCoordinatorLoaded: (data) => {
      setCoordEnabled(data.enabled);
      setCoordHasToken(data.has_token);
    },
    onDevicesLoaded: (list) => {
      setDevices(list);
    },
    onNetworkLoaded: (data) => {
      setLanEnabled(data.lan_exposure_enabled);
      setTlsActive(data.tls_active);
      setExposedInterfaces(data.exposed_interfaces);
    },
    getHostPairingVerifications,
  });

  // Per-resource readiness booleans. Controls that read or write a
  // resource must gate themselves on its `loaded` state — `idle`,
  // `loading`, and `failed` all disable, so a placeholder initial value
  // (`false` / `[]` / `{}`) can never be written to the backend as if
  // it were the real persisted state.
  const prefsLoaded = resources.preferences.status === 'loaded';
  const providersLoaded = resources.providers.status === 'loaded';
  const accountsLoaded = resources.accounts.status === 'loaded';
  const coordinatorLoaded = resources.coordinator.status === 'loaded';
  const devicesLoaded = resources.devices.status === 'loaded';
  const networkLoaded = resources.network.status === 'loaded';
  // Note (issue #601): Usage Meters moved off the Settings modal to the
  // Probe Panel's "Usage" tab. The Settings surface is now credentials +
  // harness config + LAN/Coordinator/Device toggles only. The meters
  // fetch, the meter-section JSX, and the meters-only Refresh button are
  // gone from here.
  const [accounts, setAccounts] = useState<ProviderAccount[]>([]);
  const [keyedCatalog, setKeyedCatalog] = useState<ProviderAccount[]>([]);
  const [addingProvider, setAddingProvider] = useState(false);
  // Proxied Provider pairings (ADR-0025): stored only. `storedPairingKeys`
  // marks detachable rows; `compatibleByHarness` drives the attach picker.
  const [pairings, setPairings] = useState<ProviderPairing[]>([]);
  const [pairingVerifications, setPairingVerifications] = useState<PairingVerification[]>([]);
  const [storedPairingKeys, setStoredPairingKeys] = useState<Set<string>>(new Set());
  const [compatibleByHarness, setCompatibleByHarness] = useState<Record<string, ProviderAccount[]>>({});
  const [coordEnabled, setCoordEnabled] = useState(false);
  const [coordHasToken, setCoordHasToken] = useState(false);
  const [coordToken, setCoordToken] = useState<string | null>(null);
  const [coordBusy, setCoordBusy] = useState(false);
  const [coordCopied, setCoordCopied] = useState(false);
  const [devices, setDevices] = useState<DeviceSession[]>([]);
  const [confirmingRevokeId, setConfirmingRevokeId] = useState<number | null>(null);
  const [revokingId, setRevokingId] = useState<number | null>(null);
  const [lanEnabled, setLanEnabled] = useState(false);
  const [lanBusy, setLanBusy] = useState(false);
  // Issue #1501: confirm-before-quit. The value lives in
  // `useExitPromptStore` (not local state) so the window-close guard reads
  // the same synchronous source of truth the checkbox writes — no cold IPC
  // on the close path. `true` is the default (fresh install or older
  // preferences.json without the field). Optimistic toggle with rollback,
  // same `optimisticToggle` helper as the coordinator/LAN switches.
  const confirmBeforeQuit = useExitPromptStore((s) => s.confirmBeforeQuit);
  const [confirmQuitBusy, setConfirmQuitBusy] = useState(false);
  // Issue #824: the user-configured rename backend. `null` means
  // auto-naming is OFF (the post-v2 default). Distinct from `selected`
  // above (default provider for spawn), since rename runs frequently on
  // trivial content and shouldn't inherit the node's model.
  const [namingProvider, setNamingProvider] = useState<string | null>(null);
  const [namingSaving, setNamingSaving] = useState(false);
  const [activeTab, setActiveTab] = useState<SettingsTabId>('general');
  // Autopilot pool size (app-wide cap on concurrent autopilot nodes). The
  // draft is a string so the input can hold a cleared/in-progress value;
  // `''` means "no global cap". Committed on blur / Enter rather than per
  // keystroke — a half-typed "1" of "10" must not briefly cap the pool at 1.
  const [poolDraft, setPoolDraft] = useState('');
  const [poolSaving, setPoolSaving] = useState(false);
  // Last value confirmed saved (canonical string form), for rollback and
  // dirty comparison. A ref for the same closure-staleness reason as
  // `selectedRef` (issue #581).
  const poolSavedRef = useRef('');
  // Issue #1519: Buildmesh-wide default Worktree Node directory. Draft is
  // the raw textfield value; `''` means "no app override — each Mesh uses
  // `.claude/worktrees` unless it has its own override". Committed on blur /
  // Enter like the pool size — a half-typed path must not briefly relocate
  // the warm pool.
  const [worktreeDirDraft, setWorktreeDirDraft] = useState('');
  const [worktreeDirSaving, setWorktreeDirSaving] = useState(false);
  const worktreeDirSavedRef = useRef('');
  // Issue #734: theme toggle. `themeDraft` mirrors currentTheme() so the
  // radio reflects the active value on modal open; flipping it calls
  // setTheme(), which writes localStorage, updates <html data-theme>,
  // AND fires the module-level pub/sub that every ThemeManager listens
  // to — so agent terminals and build-run terminals flip in lockstep
  // without the modal touching either registry directly. No rollback
  // path — setTheme is synchronous and writes to a synchronous
  // localStorage key, so a "failed save" isn't possible.
  const [themeDraft, setThemeDraft] = useState<ThemeName>(currentTheme);
  // Issue #1150: per-harness application defaults. The full sparse map
  // hydrates from `getAppPreferences` alongside the other settings; the
  // section re-reads via `getAppPreferences` after a successful save so a
  // backend normalise-on-write (e.g. clearing an all-blank value) is
  // reflected without a stale local cache.
  const [harnessDefaults, setHarnessDefaults] = useState<Record<string, HarnessConfigValue>>({});
  // Mirrored here so the rename picker only enables after the
  // preferences load resolves (issue #1534).
  // Realized exposure (issue #586). Mirrors `lanEnabled` (DB intent) until a
  // mismatch is detected — `lanEnabled=true` with no interfaces means the
  // toggle is on but the server is still loopback-only (TLS init failure, no
  // interface, per-interface bind error). The UI renders a warning so the
  // user knows their phone URL won't actually connect.
  const [tlsActive, setTlsActive] = useState(false);
  const [exposedInterfaces, setExposedInterfaces] = useState<RealizedBind[]>(
    [],
  );

  // Modal-wide dirty aggregator (issue #730). Every child that can be edited
  // (AccountCard, AddProviderForm, HarnessConfigList) reports via
  // `onDirtyChange(site, dirty)`; the Set's size feeds the Modal's `dirty`
  // prop so a stray Escape or backdrop click is intercepted by the inline
  // "Discard unsaved changes?" banner. The function-form setDirtySites
  // bails out when the site is already in the right state, so re-fires from
  // a non-memoised child callback are cheap.
  const [dirtySites, setDirtySites] = useState<Set<string>>(new Set());
  const siteDirtyChange = useCallback((site: string, dirty: boolean) => {
    setDirtySites(prev => {
      if (dirty) {
        if (prev.has(site)) return prev;
        const next = new Set(prev);
        next.add(site);
        return next;
      }
      if (!prev.has(site)) return prev;
      const next = new Set(prev);
      next.delete(site);
      return next;
    });
  }, []);

  // Which panes hold unsaved edits — drives the amber dot on the nav rail so
  // a dirty pane stays discoverable after the user tabs away from it.
  const dirtyPanes = useMemo(
    () => new Set([...dirtySites].map(paneForDirtySite)),
    [dirtySites],
  );

  // Issue #581: mirror `providers` and `selected` into refs so the
  // optimistic rollback handlers below read the *latest* committed value
  // instead of whichever value the render that created the closure
  // happened to hold. A closure-captured `previous = providers` goes stale
  // the moment a re-render commits a new `providers`, and two reorders
  // fired in quick succession would both roll back to the same stale
  // snapshot. Mirrors `optimisticToggle`'s "explicit current argument"
  // pattern (#587), generalised via refs for the non-toggling state.
  const providersRef = useRef<ProviderInfo[]>([]);
  useEffect(() => {
    providersRef.current = providers;
  }, [providers]);
  const selectedRef = useRef<string>(NO_OVERRIDE);
  // Mirror of `namingProvider` for the same closure-rollback reason as
  // `selectedRef` (issue #581): a rapid second change rolls back to
  // the value as of its own selection, not to a stale render snapshot.
  const namingRef = useRef<string | null>(null);
  useEffect(() => {
    selectedRef.current = selected;
  }, [selected]);

  // Re-read the realized network fields after a toggle completes so the
  // realized state (TLS active, exposed interfaces) reflects the
  // post-rebind listeners. Without this the user flips the switch, the
  // optimistic `lanEnabled` flips, but the realized fields stay stale
  // until the next modal open.
  //
  // Issue #1534 (review round 4) — this is NOT the same as
  // `loadNetwork()`. `loadNetwork()` would also overwrite `lanEnabled`
  // from the network response, racing with the optimistic toggle
  // value if a second toggle lands before the read resolves (toggle
  // A's refresh can land after toggle B's optimistic flip and revert
  // B). The optimistic `lanEnabled` is the source of truth between
  // writes; this helper deliberately skips it.
  const refreshNetworkStatus = async () => {
    try {
      const network = await api.getNetworkStatus();
      setTlsActive(network.tls_active);
      setExposedInterfaces(network.exposed_interfaces);
    } catch (e) {
      // Non-fatal — the toggle still reflects the optimistic value. Just log
      // so a future debugging session can correlate the gap.
      console.error('Failed to refresh network status:', e);
    }
  };

  // Issue #1534 (review round 5) — the 7 inline `loadX` callbacks
  // were deleted. The hook (`useSettingsResources`) owns the loaders
  // now; the modal wires its derived-state setters into the
  // hook's `onXLoaded` callbacks at hook construction.
  // Retry a single failed resource (issue #1534). The per-resource
  // status flips to `loading`, then back to `loaded` | `failed` based
  // on the read outcome. Other resources are untouched — the user
  // clicking Retry should rehydrate only what they asked for.
  //
  // Issue #1534 (review round 4) — pairings has a hard dependency
  // on providers: it queries `compatible_providers_for_harness` for
  // each non-terminal harness in the providers list. If providers
  // isn't loaded, retrying pairings would either fabricate a clean
  // state (the bug round 2 caught) or stay permanently failed (the
  // trap round 4 caught). The boundary check belongs here, not in
  // the core loader.
  //
  // Issue #1534 (review round 5) — the modal's local `retryResource`
  // was deleted. The hook's `retryResource` (returned from
  // `useSettingsResources`) handles the pairings guard internally,
  // accepts the same `(key, options?)` signature the modal's call
  // sites use, and replaces the 7-case switch with a `Record`-based
  // dispatch.

  useEffect(() => {
    // Fan out the six independent initial loads concurrently.
    // `Promise.allSettled` here is *defensive*: each `loadX` already
    // swallows its own errors via `withResourceLoad` and updates
    // `resources[key]` independently, so a rejection in one loader
    // doesn't affect the others. `allSettled` just makes the
    // intent explicit and protects against a loader that escapes
    // its catch in the future.
    //
    // Issue #1534 (review round 5) — pairings is loaded AFTER
    // providers resolves (rather than auto-chained inside
    // `loadProviders`) so that account-only handlers don't
    // re-probe WSL harness verifications. The chain here is
    // explicit so it's easy to see where pairings enters the
    // critical path: providers → pairings, not accounts →
    // providers → pairings.
    void Promise.allSettled([
      loadPreferences(),
      loadProviders(),
      loadAccounts(),
      loadCoordinator(),
      loadDevices(),
      loadNetwork(),
    ]).then(async (results) => {
      // Once providers settles on initial mount, trigger pairings.
      // We check `results` for the providers outcome so a failed
      // providers load still leaves pairings at `idle` — pairings
      // stays off the critical path until the user retries providers.
      const providersResult = results[1];
      if (providersResult.status === 'fulfilled' && providersResult.value) {
        await loadPairings(providersResult.value);
      }
    });
  }, [
    loadPreferences,
    loadProviders,
    loadAccounts,
    loadCoordinator,
    loadDevices,
    loadNetwork,
    loadPairings,
  ]);

  // Revoke a paired device: optimistically drop it from the list, then call the
  // backend (which deletes the row and force-closes any live socket it holds).
  // Roll back on failure so the list never lies about what's still authorized.
  // After a successful revoke, re-fetch the list so the panel stays authoritative
  // — a device may have re-paired, or `last_active_at` may have ticked up, while
  // the user was staring at the modal (issue #595). The refresh is best-effort:
  // a list-fetch failure after a successful revoke must NOT roll back the row,
  // because the revoke did happen — re-showing it would be a worse lie than
  // briefly stale metadata. Mirrors `refreshNetworkStatus`'s pattern (#586).
  const handleRevokeDevice = async (id: number) => {
    const previous = devices;
    setConfirmingRevokeId(null);
    setRevokingId(id);
    setError(null);
    setDevices(prev => prev.filter(d => d.id !== id));
    try {
      await api.revokeDeviceSession(id);
      // Issue #1534 (review round 4) — route the post-revoke
      // refresh through the named loader. A `listDeviceSessions`
      // failure now flips `devicesState` to `failed` and surfaces
      // the banner instead of silently leaving the post-revoke
      // list stale (the revoke DID happen — the loader just
      // rehydrates the metadata). The mutation itself still uses
      // `setError` on the optimistic-rollback path because that's
      // a transient write-failure surface, not a load failure.
      await loadDevices();
    } catch (e) {
      setDevices(previous);
      setError(formatError(e));
    } finally {
      setRevokingId(null);
    }
  };

  // Issue #1534 — `loadPairingData` is superseded by the per-resource
  // `loadPairings(providerList)` above. The legacy function silently
  // swallowed its errors as `console.error` and left the pairing state
  // empty, which the Harnesses pane then rendered as "no compatible
  // providers" — indistinguishable from "no providers actually fit".
  // The new loader routes its failure through `pairingsState` so the
  // pane can render an accessible error + Retry instead.

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen('pairing-verification-changed', () => {
      void getHostPairingVerifications().then(setPairingVerifications);
    }).then((stop) => {
      unlisten = stop;
    });
    return () => unlisten?.();
  }, []);

  // Attach a provider to a harness (ADR-0025). Client supplies base URL and
  // (for Anthropic) model tiers; backend seeds the global key set-if-absent.
  // Issue #1534 (review round 2) — mutation handlers previously
  // re-fetched providers/accounts/pairings via raw `api.listProviders()`
  // and friends, bypassing the resource-load state machine. A
  // post-mutation refresh failure used to silently leave stale data
  // with no visible signal. Now every refresh routes through the
  // named loaders so failures show in the per-resource banners
  // exactly like a fresh-modal-open failure.
  //
  // The mutation itself (e.g. `api.attachProxiedProvider`) still
  // uses `setError` — write failures are transient and the mutation
  // caller is right there to retry. Only the *refresh* goes through
  // the state machine.

  const handleAttachProvider = async (
    harnessId: string,
    providerId: string,
    apiKey: string | null,
    baseUrl: string | null,
    modelTiers: api.ModelTiers | null,
  ) => {
    setError(null);
    try {
      await api.attachProxiedProvider(harnessId, providerId, apiKey, baseUrl, modelTiers);
      // Issue #1534 (review round 5) — attach changes the
      // providers list AND the pairings (newly attached provider
      // is now in the effective pairings set), so both refreshes
      // are needed. `loadProviders` alone is no longer sufficient.
      // The chain order matters: loadProviders first (its result
      // drives pairings), then loadPairings(providers).
      const [providerList] = await Promise.all([loadProviders(), loadAccounts()]);
      if (providerList) {
        await loadPairings(providerList);
      }
    } catch (e) {
      setError(formatError(e));
      throw e;
    }
  };

  const handleUpdatePairing = async (
    harnessId: string,
    providerId: string,
    baseUrl: string | null,
    modelTiers: api.ModelTiers | null,
  ) => {
    setError(null);
    try {
      await api.updateProviderPairing(harnessId, providerId, baseUrl, modelTiers);
      // Update changes pairings too (model tiers / base URL live
      // on the pairing, not on the provider).
      const providerList = await loadProviders();
      if (providerList) {
        await loadPairings(providerList);
      }
    } catch (e) {
      setError(formatError(e));
      throw e;
    }
  };

  const handleDetachProvider = async (harnessId: string, providerId: string) => {
    setError(null);
    try {
      await api.removeProviderPairing(harnessId, providerId);
      // Detach removes the pairing — both providers and pairings
      // need refresh. Same chain as attach.
      const providerList = await loadProviders();
      if (providerList) {
        await loadPairings(providerList);
      }
    } catch (e) {
      setError(formatError(e));
      throw e;
    }
  };

  const handleVerifyPairing = async (
    harnessId: string,
    providerId: string,
    envType: api.EnvType,
  ) => {
    setError(null);
    try {
      await api.verifyProviderPairing(harnessId, providerId, envType);
      // Issue #1534 (review round 4) — verifications aren't a
      // tracked resource (they live in `pairingVerifications`
      // alongside pairings, but the modal doesn't surface them
      // independently). The full `loadPairings()` would also
      // re-query pairings + compatibility, which is wasteful for
      // a verify-only operation. We keep this as a focused,
      // best-effort side-channel refresh — a failure here falls
      // back to an empty list and the modal continues rendering
      // pairings via the resource state machine.
      setPairingVerifications(await getHostPairingVerifications());
    } catch (e) {
      setError(formatError(e));
      setPairingVerifications(await getHostPairingVerifications().catch(() => []));
      throw e;
    }
  };

  // Issue #577 — persist the per-harness Proxied Provider child order.
  // Optimistically reorders the local `pairings` slice for the affected
  // harness (cross-harness drag is disallowed at the UI layer — each
  // `HarnessCard` is its own `DndContext`, so the harnessId here always
  // matches the card the drag started in). Rolls back on failure so the
  // visible order never lies about what was stored. The backend emits
  // `provider-list-changed`, so the sidebar / probes re-read live — same
  // pattern as `handleReorderHarnesses`.
  const handleReorderProxiedProviders = async (
    harnessId: string,
    newProviderIds: string[],
  ) => {
    const previous = pairings;
    // Partition the prior pairings once: this harness's children go through
    // the reorder; every other harness's pairings stay put.
    const within = previous.filter((p) => p.harness_id === harnessId);
    const outside = previous.filter((p) => p.harness_id !== harnessId);
    // Fallback to the prior index for any id the user didn't touch (a
    // dnd-kit drag only reorders the items the user interacted with, so
    // the dragged subset is the only re-ranked input — but a paired
    // provider that's currently rendered but not in `newProviderIds`
    // would otherwise land at the very bottom by accident).
    const previousIndex = new Map(within.map((p, i) => [p.provider_id, i]));
    const rank = new Map(newProviderIds.map((id, i) => [id, i]));
    const reorderedWithin = [...within].sort(
      (a, b) =>
        (rank.get(a.provider_id) ?? previousIndex.get(a.provider_id) ?? 0) -
        (rank.get(b.provider_id) ?? previousIndex.get(b.provider_id) ?? 0),
    );
    setPairings([...outside, ...reorderedWithin]);
    setError(null);
    try {
      await api.setProxiedProviderOrder(harnessId, newProviderIds);
    } catch (e) {
      setPairings(previous);
      setError(formatError(e));
    }
  };

  // Persist a new spawn-menu harness order (issue #573). Optimistically reorder
  // the local `providers` list to match — keeping any non-listed rows (Terminal)
  // appended at the end, exactly as the backend re-derives them — and roll back
  // on failure so the visible order never lies about what was stored. The
  // backend emits `provider-list-changed`, so the sidebar / probes re-read live.
  // `previous` is read from `providersRef` (issue #581) so a handler running
  // against a stale closure still rolls back to the latest committed state.
  const handleReorderHarnesses = async (order: string[]) => {
    const previous = providersRef.current;
    const byId = new Map(previous.map(p => [p.id, p]));
    const reordered = [
      ...(order.map(id => byId.get(id)).filter(Boolean) as ProviderInfo[]),
      ...previous.filter(p => !order.includes(p.id)),
    ];
    setProviders(reordered);
    setError(null);
    try {
      await api.setHarnessOrder(order);
    } catch (e) {
      setProviders(previous);
      setError(formatError(e));
    }
  };

  // Persist the default-provider dropdown. Reads `previous` from `selectedRef`
  // (issue #581) so a rapid second change rolls back to the value as of its
  // own selection, not to a snapshot from the render that captured the first
  // change's closure.
  const handleSave = async (newValue: string) => {
    const previous = selectedRef.current;
    setSelected(newValue);
    setSaving(true);
    setError(null);
    try {
      const providerArg = newValue === NO_OVERRIDE ? null : newValue;
      await api.setAppDefaultProvider(providerArg);
      // Issue #1534 (review round 5) — refresh via the loader so a
      // failed `get_app_preferences` (or a backend normalisation)
      // flips `prefsState` to `failed` and surfaces the banner
      // rather than leaving the optimistic value lying.
      await loadPreferences();
    } catch (e) {
      setSelected(previous);
      setError(formatError(e));
    } finally {
      setSaving(false);
    }
  };

  // The reviewer default is deliberately a separate preference from the
  // ordinary spawn default. Reviews are often adversarial, so the useful
  // configuration is "use this independent Spawn Option" while preserving
  // the source-agent fallback when the selector is cleared.
  const handleSaveReviewer = async (newValue: string) => {
    const previous = reviewerProvider;
    setReviewerProvider(newValue);
    setReviewerSaving(true);
    setError(null);
    try {
      const providerArg = newValue === NO_OVERRIDE ? null : newValue;
      await api.setAppReviewerProvider(providerArg);
      await loadPreferences();
    } catch (e) {
      setReviewerProvider(previous);
      setError(formatError(e));
    } finally {
      setReviewerSaving(false);
    }
  };

  // Issue #824: persist the rename backend. Distinct from `handleSave`
  // above — auto-naming runs frequently on trivial content, so it lives
  // on its own picker with its own optimistic-rollback ref. Empty
  // string is normalised to `null` so the picker value reads as
  // "auto-naming off" rather than as some bizarre empty id.
  const handleSaveNaming = async (newValue: string | null) => {
    const previous = namingRef.current;
    const next = newValue && newValue.length > 0 ? newValue : null;
    namingRef.current = next;
    setNamingProvider(next);
    setNamingSaving(true);
    setError(null);
    try {
      await api.setAppNamingProvider(next);
      // Issue #1534 (review round 5) — backend normalises empty
      // string to `null` (and may reject unknown providers), so a
      // post-write refresh via the loader is what catches a backend
      // reject / normalisation that the optimistic update missed.
      await loadPreferences();
    } catch (e) {
      namingRef.current = previous;
      setNamingProvider(previous);
      setError(formatError(e));
    } finally {
      setNamingSaving(false);
    }
  };

  // Issue #734: persist the theme choice. `setTheme` is the single
  // entry point — it writes localStorage, sets/clears <html data-theme>,
  // and fires the module-level pub/sub that BOTH registries'
  // ThemeManager instances subscribe to. So one call here updates the
  // agent terminal AND the build/run terminal in lockstep. No rollback:
//   localStorage writes are synchronous and the DOM/xterm flips are
//   in-memory. The dirty-tracker is intentionally NOT involved — a
//   theme flip is an instant visual change with no half-saved state,
//   so a "Discard unsaved changes?" prompt would be more confusing
//   than helpful.
  const handleSaveTheme = (next: ThemeName) => {
    if (next === themeDraft) return;
    setThemeDraft(next);
    setTheme(next);
  };

  // Issue #1150 / #1148: persist one harness's application default.
  // Returns `true` on success so the section's optimistic card can commit
  // the draft into its `committed` snapshot; `false` on failure rolls
  // back the draft to the last confirmed value AND surfaces the shared
  // error banner (issue #1148 acceptance criteria 7: "On save failure,
  // retain or restore the last confirmed value and show the existing
  // settings error feedback rather than displaying an unsaved value as
  // active"). A successful save re-reads `getAppPreferences` so a
  // backend normalise-on-write (all-blank → entry removed) is reflected
  // without a stale local snapshot.
  const handleSetHarnessDefault = async (
    profileId: string,
    value: HarnessConfigValue,
  ): Promise<boolean> => {
    setError(null);
    try {
      await api.setHarnessDefault(profileId, value);
      // Issue #1534 (review round 4) — route the post-write
      // preferences refresh through the loader so a
      // `get_app_preferences` rejection flips `prefsState` to
      // `failed` and surfaces the banner, instead of leaving the
      // optimistic `harness_defaults` commit silently stale.
      // `loadPreferences` updates the modal-wide drafts (pool size,
      // worktree dir, etc.) from the canonical preferences and
      // re-reads `harness_defaults` on success.
      await loadPreferences();
      return true;
    } catch (e) {
      setError(formatError(e));
      return false;
    }
  };

  const handleClearHarnessDefault = async (profileId: string): Promise<boolean> => {
    setError(null);
    try {
      await api.clearHarnessDefault(profileId);
      // Issue #1534 (review round 5) — refresh via the loader so a
      // failed `get_app_preferences` (or a backend normalisation
      // that other harness defaults changed alongside) surfaces
      // in the preferences banner rather than leaving the optimistic
      // `harness_defaults` mutation silently stale.
      await loadPreferences();
      return true;
    } catch (e) {
      setError(formatError(e));
      return false;
    }
  };

  // Commit the worktree-directory draft (blur / Enter, issue #1519). `''`
  // (or whitespace-only) clears the app default so inheriting Meshes fall
  // back to `.claude/worktrees`; anything else stores the trimmed raw input
  // verbatim (no shell/`~` expansion — resolution joins it literally).
  // Optimistic with rollback, mirroring the pool-size write.
  const commitWorktreeDir = async () => {
    const trimmed = worktreeDirDraft.trim();
    const canonical = trimmed;
    setWorktreeDirDraft(canonical);
    if (canonical === worktreeDirSavedRef.current) {
      siteDirtyChange('worktree-dir', false);
      return;
    }
    const previous = worktreeDirSavedRef.current;
    worktreeDirSavedRef.current = canonical;
    siteDirtyChange('worktree-dir', false);
    setWorktreeDirSaving(true);
    setError(null);
    try {
      await api.setAppWorktreeDirectory(canonical === '' ? null : canonical);
      // Issue #1534 (review round 4) — route the post-write
      // preferences refresh through the loader so a rejection
      // surfaces in the preferences banner rather than leaving
      // the optimistic draft silently stale.
      await loadPreferences();
    } catch (e) {
      worktreeDirSavedRef.current = previous;
      setWorktreeDirDraft(previous);
      setError(formatError(e));
    } finally {
      setWorktreeDirSaving(false);
    }
  };

  // Commit the autopilot pool-size draft (blur / Enter). `''` clears the
  // global cap; anything else is clamped to a non-negative integer (0 =
  // pause new autopilot spawns). Optimistic with rollback, mirroring the
  // other settings writes; the dirty site clears optimistically too so a
  // successful save never leaves a phantom discard banner.
  const commitPoolSize = async () => {
    const trimmed = poolDraft.trim();
    const numeric = Number(trimmed);
    if (trimmed !== '' && Number.isNaN(numeric)) {
      // Unreachable via the DOM (type=number sanitises non-numeric input to
      // ''), but guards the IPC from ever carrying NaN if the input type
      // changes: revert to the saved value rather than sending garbage.
      setPoolDraft(poolSavedRef.current);
      siteDirtyChange('autopilot-pool', false);
      return;
    }
    const parsed = trimmed === '' ? null : Math.max(0, Math.floor(numeric));
    const canonical = parsed === null ? '' : String(parsed);
    setPoolDraft(canonical);
    if (canonical === poolSavedRef.current) {
      siteDirtyChange('autopilot-pool', false);
      return;
    }
    const previous = poolSavedRef.current;
    poolSavedRef.current = canonical;
    siteDirtyChange('autopilot-pool', false);
    setPoolSaving(true);
    setError(null);
    try {
      await api.setAppAutopilotPoolSize(parsed);
      // Issue #1534 (review round 5) — refresh via the loader so a
      // failed `get_app_preferences` (or a backend normalisation
      // e.g. server-side cap adjustment) surfaces in the preferences
      // banner rather than leaving the optimistic `autopilot_pool_size`
      // silently stale.
      await loadPreferences();
    } catch (e) {
      poolSavedRef.current = previous;
      setPoolDraft(previous);
      setError(formatError(e));
    } finally {
      setPoolSaving(false);
    }
  };

  // Persist an account, then reload the merged list so the card reflects the
  // new enabled/billing state. Rolls the local list back on failure so the
  // toggle never lies about what the backend stored.
  // Returns whether the save succeeded so callers (e.g. the add-custom form) can
  // keep their UI open on failure instead of dismissing over the error.
  //
  // Issue #601: previously also re-fetched `get_provider_meters` here so the
  // card's bars updated after a toggle. The meters no longer live on this
  // surface — they live on the Probe Panel's "Usage" tab — so this function
  // is purely an account catalogue refresh now.
  const handleSaveAccount = async (account: ProviderAccount): Promise<boolean> => {
    const previous = accounts;
    setAccounts(prev => prev.map(a => (a.id === account.id ? account : a)));
    setError(null);
    try {
      await api.upsertProviderAccount(account);
      // Issue #1534 (review round 2) — refresh through the loaders
      // so any failure surfaces in the resource banner instead of
      // silently leaving the optimistic data stale. The optimistic
      // roll-back below catches the *mutation* failure; the loader
      // catches the *refresh* failure.
      await Promise.all([loadAccounts(), loadProviders()]);
      return true;
    } catch (e) {
      setAccounts(previous);
      setError(formatError(e));
      return false;
    }
  };

  const handleRemoveAccount = async (id: string) => {
    setError(null);
    // Clear the dirty site for the card being removed BEFORE the await —
    // the form's own useEffect has no unmount cleanup, so if we wait for
    // the network round-trip the user could trigger a backdrop click that
    // surfaces the discard banner over a now-empty modal. Issue #730
    // code-review catch.
    siteDirtyChange(`account-${id}`, false);
    try {
      await api.removeProviderAccount(id);
      // Route through the loaders (issue #1534 review round 2) so
      // the providers/accounts resource states reflect the post-
      // remove snapshot. A refresh failure surfaces in the per-
      // resource banners, not as silent staleness.
      await Promise.all([loadAccounts(), loadProviders()]);
    } catch (e) {
      setError(formatError(e));
    }
  };

  const refreshAccountsAndCatalog = async () => {
    // Route through the loaders (issue #1534 review round 2) —
    // the loaders update both data and resource status atomically,
    // so a refresh failure is visible in the banner rather than
    // leaving stale data + `loaded` status.
    await Promise.all([loadAccounts(), loadProviders()]);
  };

  /** Materialise a keyed first-class template from the catalog (ADR-0025). */
  const handleAddCatalogProvider = async (template: ProviderAccount) => {
    setError(null);
    try {
      await api.upsertProviderAccount({ ...template, enabled: true });
      siteDirtyChange('add-custom-form', false);
      setAddingProvider(false);
      await refreshAccountsAndCatalog();
    } catch (e) {
      setError(formatError(e));
    }
  };

  /** Create a generic provider — name + API key only (endpoint on attach). */
  const handleAddGeneric = async (name: string, apiKey: string) => {
    const id = name.trim().toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
    if (!id) {
      setError('Custom provider needs a name');
      return;
    }
    if (accounts.some(a => a.id === id) || isSelfAuthId(id) || KEYED_FIRST_CLASS_IDS.includes(id)) {
      setError(`A provider with id "${id}" already exists`);
      return;
    }
    const ok = await handleSaveAccount({
      id,
      name: name.trim(),
      enabled: true,
      claude_compatible: true,
      billing_mode: 'pay_as_you_go',
      api_key: apiKey.trim() || null,
    });
    // Issue #1534 (review round 5) — `handleSaveAccount` already
    // refreshes accounts via `loadAccounts`, which (since the
    // round-4 catalog fix) also fetches the catalog best-effort.
    // Re-fetching the catalog here was a duplicate IPC round-trip
    // and could overwrite the loader's fresh state with stale data
    // if the loader's refresh was still in flight. Just dismiss
    // the form; the post-save loader-driven refresh covers both.
    if (ok) {
      siteDirtyChange('add-custom-form', false);
      setAddingProvider(false);
    }
  };

  // Flip the master kill-switch. Optimistic, with rollback on failure so the
  // toggle never lies about the backend's real state. The shared
  // `optimisticToggle` helper (issue #587) factors out the previous/set/try/
  // catch/finally shape that's repeated by every settings toggle.
  const handleToggleCoordinator = (enabled: boolean) =>
    optimisticToggle({
      current: coordEnabled,
      next: enabled,
      setValue: setCoordEnabled,
      setBusy: setCoordBusy,
      setError,
      // The async wrapper turns `_invoke`'s `Promise<unknown>` into
      // `Promise<void>` so it satisfies the helper's contract.
      mutation: async () => {
        await api.setCoordinatorApiEnabled(enabled);
      },
      // Issue #1534 (review round 5) — refresh via the loader so a
      // backend normalisation (e.g. `has_token` flips when the API
      // gets disabled) surfaces in the coordinator banner. The
      // refresh is best-effort: a failure here must NOT roll back
      // the successful mutation, so we swallow the error inside
      // the wrapper.
      onSuccess: () => {
        loadCoordinator().catch((err) => {
          console.warn('[AppSettings] Failed to refresh coordinator status after toggle:', err);
        });
      },
    });

  // Flip LAN/VPN exposure. The backend rebinds the listeners live (loopback
  // plain HTTP ⇄ LAN interfaces over self-signed TLS), so we await the call
  // and roll back the toggle if it fails. On success we re-read the network
  // status so the realized-state UI (`tls_active`, `exposed_interfaces`)
  // reflects the new bind (issue #586). Same `optimisticToggle` helper as
  // the coordinator toggle — the only delta is the post-success
  // `refreshNetworkStatus` hook (issue #587).
  const handleToggleLanExposure = (enabled: boolean) =>
    optimisticToggle({
      current: lanEnabled,
      next: enabled,
      setValue: setLanEnabled,
      setBusy: setLanBusy,
      setError,
      // The async wrapper turns `_invoke`'s `Promise<unknown>` into
      // `Promise<void>` so it satisfies the helper's contract.
      mutation: async () => {
        await api.setLanExposureEnabled(enabled);
      },
      onSuccess: refreshNetworkStatus,
    });

  // Issue #1501: flip the exit-confirmation prompt. Optimistic with
  // rollback so the checkbox never lies about the persisted value. The
  // store is the single writer: the close guard's synchronous read sees
  // the new value immediately, even before the backend confirms.
  const handleToggleConfirmQuit = (enabled: boolean) =>
    optimisticToggle({
      current: useExitPromptStore.getState().confirmBeforeQuit,
      next: enabled,
      setValue: (v) => useExitPromptStore.getState().setConfirmBeforeQuit(v),
      setBusy: setConfirmQuitBusy,
      setError,
      mutation: async () => {
        await api.setAppConfirmBeforeQuit(enabled);
      },
      // Issue #1534 (review round 5) — refresh via the loader so a
      // backend rejection on the post-write preferences read surfaces
      // in the preferences banner. Best-effort: a refresh failure
      // must NOT roll back the successful mutation (the value is
      // already persisted).
      onSuccess: () => {
        loadPreferences().catch((err) => {
          console.warn('[AppSettings] Failed to refresh preferences after confirm-quit toggle:', err);
        });
      },
    });

  // Mint (or replace) the read token. The value is returned exactly once, here —
  // get_coordinator_status only ever reports whether one exists, never its value.
  const handleGenerateToken = async () => {
    setCoordBusy(true);
    setCoordCopied(false);
    setError(null);
    try {
      const token = await api.generateCoordinatorReadToken();
      setCoordToken(token);
      setCoordHasToken(true);
    } catch (e) {
      setError(formatError(e));
    } finally {
      setCoordBusy(false);
    }
  };

  const handleCopyToken = async () => {
    if (!coordToken) return;
    try {
      await navigator.clipboard.writeText(coordToken);
      setCoordCopied(true);
      // Let the "Copied!" confirmation fade back so a second copy reads clearly.
      setTimeout(() => setCoordCopied(false), 2000);
    } catch (e) {
      setError(formatError(e));
    }
  };

  return (
    <Modal
      onClose={onClose}
      labelledBy="app-settings-title"
      maxWidth="max-w-4xl"
      className="p-0 max-h-[85vh] flex flex-col overflow-hidden"
      dirty={dirtySites.size > 0}
      dirtyMessage="Discard unsaved changes to your settings?"
    >
      {/* Non-scrolling header: title + close stay reachable no matter how far
          the settings body is scrolled. */}
      <div className="shrink-0 flex items-start justify-between gap-4 px-10 pt-8 pb-4 border-b border-border-subtle">
        <div>
          <h2 id="app-settings-title" className="text-2xl font-semibold text-text-primary mb-1">Settings</h2>
          <p className="text-base text-text-muted">
            Buildmesh-wide defaults. Per-mesh values in Project Settings take precedence.
          </p>
        </div>
        <ModalCloseButton onClose={onClose} label="Close settings" />
      </div>

      <div className="flex-1 flex min-h-0">
        <nav
          role="tablist"
          aria-orientation="vertical"
          aria-label="Settings sections"
          className="w-44 shrink-0 border-r border-border-subtle py-5 px-3 space-y-1 overflow-y-auto"
        >
          {SETTINGS_TABS.map(tab => (
            <button
              key={tab.id}
              type="button"
              role="tab"
              aria-selected={activeTab === tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`w-full flex items-center justify-between text-left px-3 py-2 rounded-md text-base ${
                activeTab === tab.id
                  ? 'bg-bg-card text-accent-cyan font-medium'
                  : 'text-text-secondary hover:bg-bg-card/60 hover:text-text-primary'
              }`}
            >
              <span>{tab.label}</span>
              {dirtyPanes.has(tab.id) && (
                <span
                  className="h-1.5 w-1.5 rounded-full bg-status-warning"
                  title="Unsaved changes"
                  data-testid={`settings-tab-dirty-${tab.id}`}
                />
              )}
            </button>
          ))}
        </nav>

        <div className="flex-1 overflow-y-auto px-8 pb-10 pt-6">
        {/* Shared error surface — outside the panes so a failed save is
            visible no matter which pane the user is looking at. */}
        {error && (
          <div className="mb-4 text-status-error text-base">{error}</div>
        )}

        <section
          role="tabpanel"
          aria-label="General"
          hidden={activeTab !== 'general'}
          className="space-y-8"
        >
        {/* Issue #1534 (review round 5) — the General pane now
            surfaces a loading indicator while preferences /
            providers are still loading on modal open. Without this,
            the user opens the modal, every control is silently
            disabled, and there's no signal that the data hasn't
            arrived yet — looks identical to a frozen crash. The
            indicator is `role="status"` (polite) so screen readers
            announce the transition without interrupting. */}
        {(resources.preferences.status === 'loading' ||
          resources.providers.status === 'loading') && (
          <p
            className="text-base text-text-muted"
            data-testid="resource-load-general-loading"
            role="status"
            aria-live="polite"
          >
            Loading settings…
          </p>
        )}
        {/* Issue #1534 — preferences-backed controls (default provider,
            naming backend, pool size, worktree directory, confirm-quit,
            harness defaults) all stay disabled while preferences are
            loading or have failed. The error banner above them makes
            the failure visible at the top of the pane so the user
            doesn't have to discover the disabled inputs by trial. */}
        {resources.preferences.status === 'failed' && (
          <ResourceLoadStatus
            resource="preferences"
            state={resources.preferences}
            onRetry={() => retryResource('preferences')}
          />
        )}
        {/* Issue #1534 (review round 2) — the General pane also
            surfaces a providers failure. Without this banner, a
            `listProviders` rejection silently disables the default
            provider + auto-naming controls and the user is left
            wondering why. The Providers tab also gets a banner for
            the same resource, but the General pane is the one most
            users see first. */}
        {resources.providers.status === 'failed' && (
          <ResourceLoadStatus
            resource="providers"
            state={resources.providers}
            onRetry={() => retryResource('providers')}
          />
        )}
        <div className="space-y-4">
          <label className="block text-lg font-medium text-text-secondary">
            Default provider
          </label>
          <p className="text-base text-text-muted">
            Used when a mesh has no `default_provider` of its own.
          </p>
          <select
            aria-label="Default provider"
            value={selected}
            disabled={!prefsLoaded || !providersLoaded || saving}
            onChange={e => handleSave(e.target.value)}
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          >
            <option value={NO_OVERRIDE}>Anthropic (built-in default)</option>
            {providers.map(p => (
              <option key={p.id} value={p.id}>{p.label}</option>
            ))}
          </select>
        </div>

        <div className="pt-6 border-t border-border-subtle space-y-4">
          <label className="block text-lg font-medium text-text-secondary">
            Reviewer provider
          </label>
          <p className="text-base text-text-muted">
            Used by the built-in review circuit for adversarial review. Leave it
            on the source-agent fallback to use the reviewed agent's provider.
            Authored Circuits can still override this in their reviewer node.
          </p>
          <select
            aria-label="Reviewer provider"
            value={reviewerProvider}
            disabled={!prefsLoaded || !providersLoaded || reviewerSaving}
            onChange={e => handleSaveReviewer(e.target.value)}
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          >
            <option value={NO_OVERRIDE}>Source agent provider</option>
            {providers
              .filter((p) => p.id !== 'terminal')
              .map(p => (
                <option key={p.id} value={p.id}>{p.label}</option>
              ))}
          </select>
        </div>

        {/* Issue #824: Auto-naming. Distinct from default-provider above.
            Auto-naming runs frequently on trivial content, so the user
            explicitly opts in via this picker. The helper pins a cheap
            haiku tier when "anthropic" is picked so the user's main
            subscription default is never silently inherited. Empty /
            "Disabled" leaves nodes with their random adj-adj-noun
            slugs. */}
        <div className="pt-6 border-t border-border-subtle space-y-4">
          <label className="block text-lg font-medium text-text-secondary">
            Auto-naming
          </label>
          <p className="text-base text-text-muted">
            When a node finishes a turn, Buildmesh can ask a small LLM to summarise
            the work into a slug (e.g. <code>fix-auth-flow</code>) instead of the
            default <code>bold-keen-brook</code>. Auto-naming runs frequently on
            trivial content — pick a cheap backend so an Opus-class node doesn't
            burn tokens on every rename.
          </p>
          <select
            value={namingProvider ?? ''}
            disabled={!prefsLoaded || !providersLoaded || namingSaving}
            onChange={e => handleSaveNaming(e.target.value || null)}
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          >
            <option value="">Disabled (auto-naming off)</option>
            {providers
              .filter((p) => p.id !== 'terminal')
              .map((p) => (
                <option key={p.id} value={p.id}>{p.label}</option>
              ))}
          </select>
          {namingProvider === 'anthropic' && (
            <p className="text-sm text-text-muted">
              Built-in Anthropic is pinned to a haiku tier so the rename doesn't
              inherit your main subscription default.
            </p>
          )}
          {namingProvider === null && (
            <p className="text-sm text-text-muted">
              Auto-naming is off. New nodes keep random adjective-adjective-noun
              slugs. You can always rename manually from the sidebar.
            </p>
          )}
        </div>

        <div className="pt-6 border-t border-border-subtle space-y-4">
          <label
            htmlFor="autopilot-pool-size"
            className="block text-lg font-medium text-text-secondary"
          >
            Autopilot pool size
          </label>
          <p className="text-base text-text-muted">
            The most autopilot nodes allowed to run at once across{' '}
            <span className="font-medium">all</span> meshes. Each mesh still
            respects its own concurrency limit (set in Project Settings) — this
            caps the total, so ten meshes with two slots each can't put twenty
            agents on your machine. Leave empty for no global cap; 0 pauses new
            autopilot spawns. Running nodes are never stopped — lowering the cap
            just holds new spawns until slots free up.
          </p>
          <input
            id="autopilot-pool-size"
            type="number"
            min={0}
            step={1}
            inputMode="numeric"
            aria-label="Autopilot pool size"
            placeholder="No global cap"
            value={poolDraft}
            disabled={!prefsLoaded || poolSaving}
            onChange={e => {
              setPoolDraft(e.target.value);
              siteDirtyChange('autopilot-pool', e.target.value.trim() !== poolSavedRef.current);
            }}
            onBlur={commitPoolSize}
            onKeyDown={e => {
              if (e.key === 'Enter') commitPoolSize();
            }}
            className="w-48 bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          />
        </div>

        <div className="pt-6 border-t border-border-subtle space-y-4">
          <label
            htmlFor="worktree-directory"
            className="block text-lg font-medium text-text-secondary"
          >
            Worktree directory
          </label>
          <p className="text-base text-text-muted">
            Default folder for new Worktree Nodes across{' '}
            <span className="font-medium">all</span> meshes. Relative paths
            resolve from each mesh root (e.g. <code>worktrees</code>); absolute
            paths are not allowed here because one default spans both native
            and WSL meshes — set an absolute path as a per-mesh override in
            Project Settings instead. Leave empty for{' '}
            <code>.claude/worktrees</code>. A per-mesh override
            in Project Settings takes precedence. Changing this affects future
            nodes and pre-spawn pool entries only — live nodes keep their
            existing directories.
          </p>
          <input
            id="worktree-directory"
            type="text"
            aria-label="Worktree directory"
            placeholder=".claude/worktrees"
            value={worktreeDirDraft}
            disabled={!prefsLoaded || worktreeDirSaving}
            onChange={e => {
              setWorktreeDirDraft(e.target.value);
              siteDirtyChange('worktree-dir', e.target.value.trim() !== worktreeDirSavedRef.current);
            }}
            onBlur={commitWorktreeDir}
            onKeyDown={e => {
              if (e.key === 'Enter') commitWorktreeDir();
            }}
            className="w-full bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50"
          />
        </div>

        {/* Issue #1501: exit confirmation. On by default — closing the
            window with active sessions prompts instead of terminating. */}
        <div className="pt-6 border-t border-border-subtle space-y-4">
          <label className="block text-lg font-medium text-text-secondary">
            Exiting
          </label>
          <label className="flex items-center gap-3 text-base text-text-primary cursor-pointer">
            <input
              type="checkbox"
              checked={confirmBeforeQuit}
              disabled={!prefsLoaded || confirmQuitBusy}
              onChange={e => handleToggleConfirmQuit(e.target.checked)}
              className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
            />
            <span>Confirm before quitting when agent sessions are active</span>
          </label>
          <p className="text-base text-text-muted">
            When on, closing the window while agents are running asks for
            confirmation and warns about sessions that can&apos;t resume.
          </p>
        </div>

        {/* Issue #1150 / #1148: Application-level Agent Harness defaults.
            One card per native Agent Harness, capability-gated (model
            only where `supports_model_override`, effort with the
            harness's declared vocabulary otherwise). On-dirty mirrors
            into the modal's discard-confirm via `harness-defaults`. */}
        <div className="pt-6 border-t border-border-subtle">
          <HarnessDefaultsSection
            providers={providers}
            defaults={harnessDefaults}
            onChange={handleSetHarnessDefault}
            onReset={handleClearHarnessDefault}
            onDirtyChange={(d) => siteDirtyChange('harness-defaults', d)}
            disabled={!prefsLoaded}
          />
        </div>

        <div className="pt-6 border-t border-border-subtle space-y-4">
          <label
            htmlFor="theme-radio-group"
            className="block text-lg font-medium text-text-secondary"
          >
            Appearance
          </label>
          <p className="text-base text-text-muted">
            Pick the colour theme. Dark is the default; light inverts the
            surface and text tokens while keeping the accent palette intact.
            The choice is saved per machine — xterm.js terminals flip with
            the rest of the app.
          </p>
          <fieldset
            id="theme-radio-group"
            aria-label="Theme"
            className="flex flex-wrap gap-2"
          >
            {(['dark', 'light'] as const).map((name) => (
              <label
                key={name}
                className={`flex items-center gap-2 px-4 py-2 rounded-md text-base cursor-pointer border transition-colors ${
                  themeDraft === name
                    ? 'bg-bg-card border-accent-cyan text-text-primary'
                    : 'bg-bg-card border-border-subtle text-text-secondary hover:border-border-default'
                }`}
              >
                <input
                  type="radio"
                  name="theme"
                  value={name}
                  checked={themeDraft === name}
                  // Controlled radio: the picker is always in step with the
                  // active theme (setTheme is synchronous). Each click
                  // commits immediately — no "Save" button, no dirty site,
                  // no rollback. The visual transition is the persistence.
                  onChange={() => handleSaveTheme(name)}
                  className="accent-accent-cyan"
                  data-testid={`theme-radio-${name}`}
                />
                <span className="capitalize">{name}</span>
              </label>
            ))}
          </fieldset>
        </div>

        <div className="pt-6 border-t border-border-subtle">
          <p className="text-base text-text-muted">
            Provider defaults are stored in your app data directory at{' '}
            <span className="font-mono">preferences.json</span>; coordinator settings and
            authorized devices live in the app database.
          </p>
        </div>

        {/* Issue #1526 — manual update surface. The auto-launch prompt
            (UpdatePrompt) handles nag-style flow; Settings exposes
            current version + manual check + install progress for users
            who want to drive it themselves. */}
        <UpdateAboutSection />
        </section>

        <section
          role="tabpanel"
          aria-label="Harnesses"
          hidden={activeTab !== 'harnesses'}
          className="space-y-8"
        >
        {providers.filter(p => p.id !== 'terminal').length >= 2 && (
          <div className="pt-6 border-t border-border-subtle first:pt-0 first:border-t-0">
            <h3 className="text-xl font-semibold text-text-primary mb-2">Spawn menu order</h3>
            <p className="text-base text-text-muted mb-4">
              Drag to reorder how harnesses appear in every spawn menu. Terminal stays pinned last.
            </p>
            <HarnessOrderList providers={providers} onReorder={handleReorderHarnesses} />
          </div>
        )}

        <div className="pt-6 border-t border-border-subtle first:pt-0 first:border-t-0">
          <h3 className="text-xl font-semibold text-text-primary mb-2">Harnesses & proxied providers</h3>
          <p className="text-base text-text-muted mb-4">
            Proxy a model provider through a harness over a compatible API surface
            (e.g. MiniMax via Claude Code over Anthropic, or via Codex over OpenAI).
            Set the provider&apos;s API key on the Providers page; set base URL and
            Claude model names here when attaching.
          </p>
          {/* Issue #1534 (review round 2) — `idle` is now an explicit
              branch. Without it, an in-flight tab switch to the
              Harnesses pane (while `pairings` was still waiting on
              `providers`) fell through to the empty `HarnessConfigList`
              and flashed "no compatible providers" — the exact bug
              #1534 aimed to fix. Order of checks:
                1. providers failed      → providers error + retry
                2. pairings failed       → pairings error + retry
                3. pairings idle/loading → "Loading pairings…" (covers
                                           the initial-load window)
                4. otherwise             → HarnessConfigList (real data) */}
          {resources.providers.status === 'failed' ? (
            <ResourceLoadStatus
              resource="providers"
              state={resources.providers}
              onRetry={() => retryResource('providers')}
            />
          ) : resources.pairings.status === 'failed' ? (
            <ResourceLoadStatus
              resource="pairings"
              state={resources.pairings}
              onRetry={() => retryResource('pairings')}
            />
          ) : resources.pairings.status === 'idle' || resources.pairings.status === 'loading' ? (
            <ResourceLoadStatus
              resource="pairings"
              state={resources.pairings}
            />
          ) : (
            <HarnessConfigList
              harnesses={
                providers
                  .filter((p) => !p.is_proxied && p.id !== 'terminal')
                  .map((p) => ({ id: p.id, label: p.label })) as ProxyHarness[]
              }
              compatibleByHarness={compatibleByHarness}
              pairings={pairings}
              verifications={pairingVerifications}
              supportsWsl={isWindows}
              storedKeys={storedPairingKeys}
              accounts={accounts}
              onAttach={handleAttachProvider}
              onUpdate={handleUpdatePairing}
              onDetach={handleDetachProvider}
              onVerify={handleVerifyPairing}
              onReorderProxied={handleReorderProxiedProviders}
              onDirtyChange={(site, d) => siteDirtyChange(`harness-${site}`, d)}
            />
          )}
        </div>
        </section>

        <section
          role="tabpanel"
          aria-label="Providers"
          hidden={activeTab !== 'providers'}
        >
        <div>
          <h3 className="text-xl font-semibold text-text-primary mb-2">Providers</h3>
          <p className="text-base text-text-muted mb-4">
            Self-auth brands always appear (enable + billing). Add keyed first-class
            or generic providers for API keys. Base URL and model names are configured
            when you attach a provider under Harnesses. Usage Meters live on the{' '}
            <span className="font-medium">Usage</span> tab in the side panel.
          </p>

          {/* Issue #1534 — replace the empty-cards state with an
              explicit error when the accounts fetch failed. Without
              this the pane would silently show "no providers" and the
              + Add provider button as if the catalogue were simply
              empty, when in reality we don't know. OpenCode lives in
              its own sub-section that talks to its own commands, so it
              renders unconditionally below. */}
          {!accountsLoaded ? (
            <ResourceLoadStatus
              resource="accounts"
              state={resources.accounts}
              onRetry={() => retryResource('accounts')}
            />
          ) : (
            <div className="space-y-4">
              {accounts.map(account => (
                <AccountCard
                  key={account.id}
                  account={account}
                  onSave={handleSaveAccount}
                  onRemove={handleRemoveAccount}
                  onDirtyChange={d => siteDirtyChange(`account-${account.id}`, d)}
                />
              ))}
            </div>
          )}

          <div className="pt-6 border-t border-border-subtle space-y-4">
            <OpenCodeAccountCard />
          </div>

          {addingProvider ? (
            <AddProviderForm
              catalog={keyedCatalog.filter((t) => !accounts.some((a) => a.id === t.id))}
              onAddCatalog={handleAddCatalogProvider}
              onAddGeneric={handleAddGeneric}
              onCancel={() => {
                siteDirtyChange('add-custom-form', false);
                setAddingProvider(false);
              }}
              onDirtyChange={d => siteDirtyChange('add-custom-form', d)}
            />
          ) : (
            <button
              onClick={() => setAddingProvider(true)}
              className="mt-4 text-base text-text-secondary hover:text-text-primary"
            >
              + Add provider
            </button>
          )}
        </div>
        </section>

        <section
          role="tabpanel"
          aria-label="Remote Access"
          hidden={activeTab !== 'remote'}
          className="space-y-8"
        >
        <div>
          <h3 className="text-xl font-semibold text-text-primary mb-2">LAN / VPN Exposure</h3>
          <p className="text-base text-text-muted mb-4">
            Off by default — the server is reachable only from this machine
            (loopback). Enable to let a phone on your LAN or VPN connect. Exposed
            interfaces are served over HTTPS/WSS with a <span className="font-medium">self-signed
            certificate</span>, so your browser will warn the first time you connect;
            loopback stays plain HTTP. The change applies immediately — any
            currently-connected LAN device must reconnect over HTTPS.
          </p>

          {/* Issue #1534 — surface network-status failures at the top of
              the LAN section. Without this, the toggle below would be
              disabled with `lanEnabled=false`, and the user might read
              that as "LAN exposure is currently off" — when actually we
              don't know what the persisted value is. The error banner
              makes the unknown state visible. */}
          {!networkLoaded && (
            <div className="mb-4">
              <ResourceLoadStatus
                resource="network"
                state={resources.network}
                onRetry={() => retryResource('network')}
              />
            </div>
          )}

          <label className="flex items-center gap-3 text-lg text-text-primary cursor-pointer">
            <input
              type="checkbox"
              checked={lanEnabled}
              disabled={!networkLoaded || lanBusy}
              onChange={e => handleToggleLanExposure(e.target.checked)}
              className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
            />
            <span>Expose to LAN / VPN over self-signed TLS</span>
          </label>

          {/* Realized exposure (issue #586). When the toggle is on but the
              server is still loopback-only (TLS init failed, no interface,
              or per-interface bind failed), warn so the user doesn't hand
              their phone a dead URL. When exposure is working, list the
              actually-bound addresses so the user knows what to type. */}
          {lanEnabled && networkLoaded && (
            <div className="mt-4" data-testid="lan-realized-status">
              {exposedInterfaces.length === 0 ? (
                <div
                  className="flex items-start gap-2 bg-bg-card border border-status-warning/40 rounded-md px-3 py-2"
                  data-testid="lan-exposure-warning"
                  role="alert"
                >
                  <span className="text-base text-status-warning">
                    <span className="font-medium">No interfaces are actually exposed.</span>{' '}
                    The toggle is on, but the server didn’t bind any LAN address —
                    either this machine has no non-loopback interface, or the
                    self-signed certificate couldn’t be initialized. Check the
                    application log for details. The loopback listener is still
                    serving plain HTTP for this machine.
                  </span>
                </div>
              ) : (
                <div className="border border-border-subtle rounded-lg p-4">
                  <div className="text-base text-text-secondary mb-2">
                    Reach the hub from a phone on the same network at:
                  </div>
                  <ul className="font-mono text-base text-text-primary space-y-1">
                    {exposedInterfaces.map((bind) => (
                      <li key={bind.address} data-testid="lan-exposed-interface">
                        <span className="font-medium">{bind.address}</span>
                        <span className="ml-2 text-text-muted">
                          ({bind.tls ? 'HTTPS/WSS' : 'plain'})
                        </span>
                      </li>
                    ))}
                  </ul>
                  {!tlsActive && (
                    <div
                      className="mt-3 text-base text-status-warning"
                      data-testid="lan-tls-warning"
                      role="alert"
                    >
                      TLS is not active on any exposed interface — connections
                      from your phone will not be encrypted. Check the
                      application log for the certificate initialization error.
                    </div>
                  )}
                </div>
              )}
            </div>
          )}
        </div>

        <div className="pt-6 border-t border-border-subtle">
          <h3 className="text-xl font-semibold text-text-primary mb-2">Coordinator Read API</h3>
          <p className="text-base text-text-muted mb-4">
            A read-only HTTP view of every node's status for an external coordinator.
            Off by default. It binds to loopback and your LAN only — reaching it from
            anywhere else is your own tunnel (Tailscale, Cloudflare, WireGuard).
          </p>

          {/* Issue #1534 — mirror the LAN section: an unknown coordinator
              status must not look like "off" to the user. The toggle is
              already disabled (sees `!coordinatorLoaded`) but the banner
              makes the load failure visible at the section's top. */}
          {!coordinatorLoaded && (
            <div className="mb-4">
              <ResourceLoadStatus
                resource="coordinator"
                state={resources.coordinator}
                onRetry={() => retryResource('coordinator')}
              />
            </div>
          )}

          <label className="flex items-center gap-3 text-lg text-text-primary cursor-pointer">
            <input
              type="checkbox"
              checked={coordEnabled}
              disabled={!coordinatorLoaded || coordBusy}
              onChange={e => handleToggleCoordinator(e.target.checked)}
              className="accent-accent-cyan h-4 w-4 disabled:opacity-50"
            />
            <span>Enable coordinator read API</span>
          </label>

          {coordEnabled && (
            <div className="mt-4 border border-border-subtle rounded-lg p-5">
              <div className="flex items-start gap-2 bg-bg-card border border-border-strong rounded-md px-3 py-2 mb-4">
                <span className="text-base text-text-secondary">
                  Anyone who can reach this machine on the LAN <span className="font-medium">and</span>{' '}
                  holds the token can read your node statuses. The token is shown once, when
                  minted — copy it now; regenerating invalidates the old one.
                </span>
              </div>

              <div className="flex gap-3">
                <input
                  type="text"
                  readOnly
                  value={coordToken ?? (coordHasToken ? '••••••••  (a token has already been minted)' : '')}
                  placeholder="No token yet — generate one to copy"
                  className="flex-1 bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base font-mono text-text-primary focus:outline-none focus:border-accent-cyan"
                />
                {coordToken && (
                  <button
                    onClick={handleCopyToken}
                    className="px-5 py-2.5 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30"
                  >
                    {coordCopied ? 'Copied!' : 'Copy'}
                  </button>
                )}
                <button
                  onClick={handleGenerateToken}
                  disabled={coordBusy}
                  className="px-5 py-2.5 bg-accent-cyan/20 text-accent-cyan text-base rounded-md hover:bg-accent-cyan/30 disabled:opacity-50 whitespace-nowrap"
                >
                  {coordBusy ? 'Working…' : coordHasToken ? 'Regenerate token' : 'Generate token'}
                </button>
              </div>
            </div>
          )}
        </div>

        <div className="pt-6 border-t border-border-subtle">
          <h3 className="text-xl font-semibold text-text-primary mb-2">Authorized Devices</h3>
          <p className="text-base text-text-muted mb-4">
            Phones you've paired keep their own session token, so they stay
            connected as their network (and IP) changes. Revoke any device to cut
            it off immediately — its open connections drop and it must pair again
            with a fresh QR code.
          </p>

          {!devicesLoaded ? (
            <ResourceLoadStatus
              resource="devices"
              state={resources.devices}
              onRetry={() => retryResource('devices')}
            />
          ) : devices.length === 0 ? (
            <p className="text-base text-text-muted italic">
              No paired devices yet. Scan the Remote Access QR code from a phone to pair one.
            </p>
          ) : (
            <ul className="flex flex-col gap-2">
              {devices.map(device => (
                <li
                  key={device.id}
                  className="flex items-center gap-4 border border-border-subtle rounded-lg px-4 py-3"
                >
                  <div className="min-w-0 flex-1">
                    <div className="text-base text-text-primary truncate">
                      {device.label ?? 'Unknown device'}
                    </div>
                    <div className="text-sm text-text-muted truncate">
                      {device.last_ip ?? 'IP unknown'} · last active {device.last_active_at}
                    </div>
                  </div>
                  {confirmingRevokeId === device.id ? (
                    <div className="flex gap-2 whitespace-nowrap">
                      <button
                        onClick={() => handleRevokeDevice(device.id)}
                        disabled={revokingId === device.id}
                        className="px-4 py-2 bg-status-error text-white text-base rounded-md hover:bg-status-error/90 disabled:opacity-50"
                      >
                        {revokingId === device.id ? 'Revoking…' : 'Confirm revoke'}
                      </button>
                      <button
                        onClick={() => setConfirmingRevokeId(null)}
                        className="px-4 py-2 bg-bg-card text-text-secondary text-base rounded-md hover:bg-border-subtle"
                      >
                        Cancel
                      </button>
                    </div>
                  ) : (
                    <button
                      onClick={() => setConfirmingRevokeId(device.id)}
                      className="px-4 py-2 bg-status-error/15 text-status-error text-base rounded-md hover:bg-status-error/25 whitespace-nowrap"
                    >
                      Revoke
                    </button>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
        </section>
        </div>
      </div>
    </Modal>
  );
}
