import { useEffect, useId, useState } from 'react';
import type { SpawnConfiguration } from '../../types/generated/SpawnConfiguration';
import type { LaunchTarget } from '../../types/generated/LaunchTarget';
import type { ProviderPairing } from '../../types/generated/ProviderPairing';
import type { PairingVerification } from '../../types/generated/PairingVerification';
import './launchConfigurations.css';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { EffortGuidance } from '../shared/EffortGuidance';
import { Spinner } from '../shared/Spinner';
import { useHarnessLogins } from '../../hooks/useHarnessLogins';

// Verify, save, and delete share the fieldset lock; status text is derived separately.
type BusyAction = 'save' | 'verify' | 'delete';

// A clone is a new recipe seeded from an existing one: the empty id makes the
// save insert rather than overwrite, and the generated ownership and resolved
// snapshot belong to the original.
export function cloneConfiguration(value: SpawnConfiguration): SpawnConfiguration {
  return { ...value, id: '', name: `${value.name} copy`, generated: undefined, resolved: undefined };
}

export function LaunchConfigurationEditor({ value, targets, onSave, onCancel, onDelete, onDirtyChange, onVerify }: {
  value: SpawnConfiguration;
  targets: LaunchTarget[];
  onSave: (value: SpawnConfiguration, route?: ProviderPairing, onRefreshing?: () => void) => Promise<void>;
  onVerify?: (value: SpawnConfiguration, route?: ProviderPairing) => Promise<PairingVerification>;
  onCancel: () => void;
  onDelete?: () => Promise<void>;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const [draft, setDraft] = useState(value);
  const [busyAction, setBusyAction] = useState<BusyAction | null>(null);
  const [saveStage, setSaveStage] = useState<'saving' | 'refreshing'>('saving');
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [endpoint, setEndpoint] = useState<string | null>(null);
  const [customModel, setCustomModel] = useState(false);
  // Issue #2423: the raw inputs sit behind one Advanced options disclosure. It
  // opens when the configuration already carries an advanced value, and it stays
  // open while a required custom model is selected, so the disclosure stays
  // open over the field that blocks a save.
  const [advancedOpen, setAdvancedOpen] = useState(() => !!value.extra_args?.trim());
  const [advancedNotice, setAdvancedNotice] = useState(false);
  const [verification, setVerification] = useState<{ inputs: string; message: string } | null>(null);
  const advancedId = useId();
  const advancedToggleId = `${advancedId}-toggle`;
  const advancedHelpId = `${advancedId}-help`;
  const advancedExpanded = advancedOpen || customModel;
  const dirty = JSON.stringify(draft) !== JSON.stringify(value) || !value.id || endpoint !== null;
  useEffect(() => {
    onDirtyChange?.(dirty);
    return () => onDirtyChange?.(false);
  }, [dirty, onDirtyChange]);
  const target = targets.find((t) => t.id === draft.spawn_option_id);
  // A login can be pinned only on a native harness that has more than one
  // login; a Provider Route signs in with its own key.
  const logins = useHarnessLogins();
  const harnessLogins = target && !target.route ? logins.filter((login) => login.harness_id === target.harness_id) : [];
  const pinnedLoginMissing = !!draft.login_id && !harnessLogins.some((login) => login.id === draft.login_id);
  const harnesses = [...new Map(targets.map((t) => [t.harness_id, t.harness_name])).entries()];
  const model = target?.models.find((m) => m.id === (draft.model ?? target.default_model));
  const efforts = model?.efforts ?? target?.efforts ?? [];
  const route = target?.route && !target.route_attached ? { ...target.route,
    base_url: endpoint ?? target.route.base_url,
    model_tiers: { ...target.route.model_tiers, default: target.route.model_tiers.default ?? draft.model },
  } : undefined;
  const inputs = JSON.stringify([draft, route, target?.route]);
  const canSubmit = !!target && !!draft.name.trim() && (!route || !!route.base_url?.trim())
    && (!customModel || !!draft.model?.trim()) && (!target.route || !!(draft.model ?? target.default_model ?? target.route.model_tiers.default)?.trim());
  const runBusyAction = async (action: () => Promise<void>, kind: BusyAction) => {
    setBusyAction(kind);
    setError(null);
    try { await action(); } catch (e) { setError(String(e)); } finally { setBusyAction(null); }
  };
  const statusMessage = busyAction === 'save'
    ? saveStage === 'refreshing' ? 'Refreshing availability…' : 'Saving configuration…'
    : busyAction === 'verify' ? 'Verifying provider and model…'
    : busyAction === 'delete' ? 'Deleting configuration…'
    : verification
      ? verification.inputs === inputs ? verification.message : 'Configuration changed; verify again to confirm it still works.'
      : '';
  const selectTarget = (id: string) => {
    setDraft({ ...draft, spawn_option_id: id, harness_id: undefined, provider_route_id: undefined, model: null, effort: null, extra_args: null, login_id: undefined });
    setEndpoint(null); setCustomModel(false); setVerification(null); setAdvancedNotice(false);
  };
  // A selected custom model makes its input required, so the disclosure refuses
  // to close over it and says why instead of hiding a blocking field.
  const toggleAdvanced = () => {
    if (advancedExpanded && customModel) { setAdvancedNotice(true); return; }
    setAdvancedNotice(false);
    setAdvancedOpen(open => !open);
  };
  return <form className="launch-config-form" aria-label="Launch Configuration" onSubmit={(e) => {
    e.preventDefault();
    if (canSubmit) {
      setSaveStage('saving');
      void runBusyAction(() => onSave(draft, route, () => setSaveStage('refreshing')), 'save');
    }
  }}>
    <p>{draft.generated && !draft.generated.user_owned ? 'Generated configuration. Saving an edit preserves your choices against catalogue updates.' : 'User-owned configuration'}</p>
    <fieldset disabled={busyAction !== null}>
      <label>Name<input autoFocus required value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} /></label>
      <label>Harness<select value={target?.harness_id ?? ''} onChange={(e) => {
        const match = targets.find((t) => t.harness_id === e.target.value);
        if (match) selectTarget(match.id);
      }}>
        {!target && <option value="">Select a harness</option>}
        {harnesses.map(([id, name]) => <option key={id} value={id}>{name}</option>)}
      </select></label>
      <label>Provider<select value={target?.id ?? ''} onChange={(e) => selectTarget(e.target.value)}>
        {!target && <option value="">Select a provider</option>}
        {targets.filter((t) => t.harness_id === target?.harness_id).map((t) => <option key={t.id} value={t.id}>{t.provider_name}</option>)}
      </select></label>
      {(harnessLogins.length > 1 || draft.login_id) && <label>Login<select value={draft.login_id ?? ''} onChange={(e) => setDraft({ ...draft, login_id: e.target.value || undefined })}>
        <option value="">Login picked in the spawn menu</option>
        {harnessLogins.filter((login) => !login.is_default).map((login) => <option key={login.id} value={login.id}>{login.email ? `${login.label} (${login.email})` : login.label}</option>)}
        {pinnedLoginMissing && <option value={draft.login_id}>{draft.login_id} (removed)</option>}
      </select></label>}
      {route && <>
        <p>This provider will be paired with {target?.harness_name} when you save. Credentials come from Providers.</p>
        <label>Provider endpoint<input required type="url" value={route.base_url ?? ''} onChange={(e) => setEndpoint(e.target.value)} placeholder="https://provider.example/v1" /></label>
      </>}
      {target?.supports_model && <label>Model{target.manual_model ?
        <input required={!!target.route && !target.default_model} value={draft.model ?? ''} placeholder={target.default_model ?? 'Default'} onChange={(e) => setDraft({ ...draft, model: e.target.value || null, effort: null })} /> :
        <select value={customModel ? '__custom__' : draft.model ?? ''} onChange={(e) => {
          setCustomModel(e.target.value === '__custom__');
          // Picking a custom model opens the disclosure: its input is required
          // and lives inside.
          if (e.target.value === '__custom__') setAdvancedOpen(true);
          setAdvancedNotice(false);
          setDraft({ ...draft, model: e.target.value === '__custom__' ? null : e.target.value || null, effort: null });
        }}>
          <option value="">{target.default_model ? `Default (${target.default_model})` : 'Default'}</option>
          {draft.model && !model && <option value={draft.model}>{draft.model} (advanced route)</option>}
          {target.models.map((m) => <option key={m.id} value={m.id}>{m.name} ({m.id})</option>)}
          <option value="__custom__">Custom model...</option>
        </select>}
      </label>}
      {efforts.length > 0 && <>
        <label>Effort<select aria-describedby={efforts.includes('max') ? 'launch-config-effort-help' : undefined} value={draft.effort ?? ''} onChange={(e) => setDraft({ ...draft, effort: e.target.value || null })}>
          <option value="">Default</option>{efforts.map((effort) => <option key={effort} value={effort}>{effort}</option>)}
        </select></label>
        <EffortGuidance id="launch-config-effort-help" efforts={efforts} selectedEffort={draft.effort} />
      </>}
      {target?.route && efforts.length === 0 && <p>No configurable effort is documented for this model through this harness.</p>}
      {(target?.supports_extra_args || customModel) && <section className="launch-config-advanced">
        <button type="button" id={advancedToggleId} className="launch-config-advanced-toggle"
          aria-expanded={advancedExpanded} aria-controls={advancedId} onClick={toggleAdvanced}>
          <span aria-hidden="true" className="launch-config-advanced-chevron">▸</span>
          Advanced options
        </button>
        <p id={advancedHelpId} className="launch-config-advanced-help">Only needed when a provider or harness wants a raw value the fields above do not offer: a custom model reference, and extra arguments passed to the harness command line unchanged.</p>
        {advancedNotice && <p role="alert" className="launch-config-advanced-notice">A custom model is required, so advanced options stay open until you pick a listed model.</p>}
        <div id={advancedId} role="group" aria-labelledby={advancedToggleId} aria-describedby={advancedHelpId}
          hidden={!advancedExpanded} className="launch-config-advanced-fields">
          {customModel && <label>Custom model<input required value={draft.model ?? ''} onChange={(e) => setDraft({ ...draft, model: e.target.value || null, effort: null })} /></label>}
          {target?.supports_extra_args && <label>Extra arguments<input value={draft.extra_args ?? ''} onChange={(e) => setDraft({ ...draft, extra_args: e.target.value || null })} /></label>}
        </div>
      </section>}
      {target?.verification_required && onVerify && <>
        <p>Verify the selected endpoint and model before launching. Verification sends a small tool-call request using your provider credential.</p>
        <button type="button" disabled={!canSubmit} onClick={() => void runBusyAction(async () => {
          const result = await onVerify(draft, route);
          if (result.status !== 'verified') throw new Error(result.reason ?? 'Provider verification failed');
          setVerification({ inputs, message: `Verified ${result.model_id} for ${result.runtime}` });
        }, 'verify')}>Verify provider and model</button>
      </>}
      <div className="launch-config-actions">
        <button type="submit" disabled={!canSubmit || busyAction !== null}>{busyAction === 'save' ? 'Saving…' : 'Save'}</button>
        <button type="button" onClick={onCancel}>Cancel</button>
        {onDelete && <button type="button" onClick={() => setConfirmDelete(true)}>Delete</button>}
      </div>
    </fieldset>
    <div className="launch-config-status" role="status" aria-live="polite" aria-atomic="true">
      {busyAction !== null && <Spinner className="w-3.5 h-3.5 shrink-0" />}
      <span>{statusMessage}</span>
    </div>
    {error && <p role="alert">{error}</p>}
    {confirmDelete && onDelete && <ConfirmDialog className="launch-delete-dialog" title="Delete Launch Configuration?"
      message="Existing nodes keep their saved launch settings. Generated recipes will not be recreated automatically."
      onCancel={() => setConfirmDelete(false)} onConfirm={() => { setConfirmDelete(false); void runBusyAction(onDelete, 'delete'); }} />}
  </form>;
}
