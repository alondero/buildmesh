import { useEffect, useState } from 'react';
import type { SpawnConfiguration } from '../../types/generated/SpawnConfiguration';
import type { LaunchTarget } from '../../types/generated/LaunchTarget';
import type { ProviderPairing } from '../../types/generated/ProviderPairing';
import type { PairingVerification } from '../../types/generated/PairingVerification';
import './launchConfigurations.css';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';

export function LaunchConfigurationEditor({ value, targets, onSave, onCancel, onDelete, onDirtyChange, onVerify }: {
  value: SpawnConfiguration;
  targets: LaunchTarget[];
  onSave: (value: SpawnConfiguration, route?: ProviderPairing) => Promise<void>;
  onVerify?: (value: SpawnConfiguration, route?: ProviderPairing) => Promise<PairingVerification>;
  onCancel: () => void;
  onDelete?: () => Promise<void>;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const [draft, setDraft] = useState(value);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [endpoint, setEndpoint] = useState<string | null>(null);
  const [customModel, setCustomModel] = useState(false);
  const [verification, setVerification] = useState<{ inputs: string; message: string } | null>(null);
  const dirty = JSON.stringify(draft) !== JSON.stringify(value) || !value.id || endpoint !== null;
  useEffect(() => {
    onDirtyChange?.(dirty);
    return () => onDirtyChange?.(false);
  }, [dirty, onDirtyChange]);
  const target = targets.find((t) => t.id === draft.spawn_option_id);
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
  const act = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try { await action(); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const selectTarget = (id: string) => {
    setDraft({ ...draft, spawn_option_id: id, harness_id: undefined, provider_route_id: undefined, model: null, effort: null, extra_args: null });
    setEndpoint(null); setCustomModel(false); setVerification(null);
  };
  return <form className="launch-config-form" aria-label="Launch Configuration" onSubmit={(e) => { e.preventDefault(); if (canSubmit) void act(() => route ? onSave(draft, route) : onSave(draft)); }}>
    <p>{draft.generated && !draft.generated.user_owned ? 'Generated configuration. Saving an edit preserves your choices against catalogue updates.' : 'User-owned configuration'}</p>
    <fieldset disabled={busy}>
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
      {route && <>
        <p>This provider will be paired with {target?.harness_name} when you save. Credentials come from Providers.</p>
        <label>Provider endpoint<input required type="url" value={route.base_url ?? ''} onChange={(e) => setEndpoint(e.target.value)} placeholder="https://provider.example/v1" /></label>
      </>}
      {target?.supports_model && <label>Model{target.manual_model ?
        <input required={!!target.route && !target.default_model} value={draft.model ?? ''} placeholder={target.default_model ?? 'Default'} onChange={(e) => setDraft({ ...draft, model: e.target.value || null, effort: null })} /> :
        <select value={customModel ? '__custom__' : draft.model ?? ''} onChange={(e) => {
          setCustomModel(e.target.value === '__custom__');
          setDraft({ ...draft, model: e.target.value === '__custom__' ? null : e.target.value || null, effort: null });
        }}>
          <option value="">{target.default_model ? `Default (${target.default_model})` : 'Default'}</option>
          {draft.model && !model && <option value={draft.model}>{draft.model} (advanced route)</option>}
          {target.models.map((m) => <option key={m.id} value={m.id}>{m.name} ({m.id})</option>)}
          <option value="__custom__">Custom model...</option>
        </select>}
      </label>}
      {customModel && <label>Custom model<input required value={draft.model ?? ''} onChange={(e) => setDraft({ ...draft, model: e.target.value || null, effort: null })} /></label>}
      {efforts.length > 0 && <label>Effort<select value={draft.effort ?? ''} onChange={(e) => setDraft({ ...draft, effort: e.target.value || null })}>
        <option value="">Default</option>{efforts.map((effort) => <option key={effort} value={effort}>{effort}</option>)}
      </select></label>}
      {target?.route && efforts.length === 0 && <p>No configurable effort is documented for this model through this harness.</p>}
      {target?.verification_required && onVerify && <>
        <p>Verify the selected endpoint and model before launching. Verification sends a small tool-call request using your provider credential.</p>
        <button type="button" disabled={!canSubmit} onClick={() => void act(async () => {
          const result = await onVerify(draft, route);
          if (result.status !== 'verified') throw new Error(result.reason ?? 'Provider verification failed');
          setVerification({ inputs, message: `Verified ${result.model_id} for ${result.runtime}` });
        })}>Verify provider and model</button>
        {verification && <p role="status">{verification.inputs === inputs ? verification.message : 'Configuration changed; verify again before launching.'}</p>}
      </>}
      {target?.supports_extra_args && <label>Extra arguments<input value={draft.extra_args ?? ''} onChange={(e) => setDraft({ ...draft, extra_args: e.target.value || null })} /></label>}
      <div className="launch-config-actions">
        <button type="submit" disabled={!canSubmit}>Save</button>
        <button type="button" onClick={onCancel}>Cancel</button>
        {onDelete && <button type="button" onClick={() => setConfirmDelete(true)}>Delete</button>}
      </div>
    </fieldset>
    {error && <p role="alert">{error}</p>}
    {confirmDelete && onDelete && <ConfirmDialog className="launch-delete-dialog" title="Delete Launch Configuration?"
      message="Existing nodes keep their saved launch settings. Generated recipes will not be recreated automatically."
      onCancel={() => setConfirmDelete(false)} onConfirm={() => { setConfirmDelete(false); void act(onDelete); }} />}
  </form>;
}
