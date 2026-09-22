import { useEffect, useState } from 'react';
import type { SpawnConfiguration } from '../../types/generated/SpawnConfiguration';
import type { LaunchTarget } from '../../types/generated/LaunchTarget';
import './launchConfigurations.css';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';

export function LaunchConfigurationEditor({ value, targets, onSave, onCancel, onDelete, onDirtyChange }: {
  value: SpawnConfiguration;
  targets: LaunchTarget[];
  onSave: (value: SpawnConfiguration) => Promise<void>;
  onCancel: () => void;
  onDelete?: () => Promise<void>;
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const [draft, setDraft] = useState(value);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const dirty = JSON.stringify(draft) !== JSON.stringify(value) || !value.id;
  useEffect(() => {
    onDirtyChange?.(dirty);
    return () => onDirtyChange?.(false);
  }, [dirty, onDirtyChange]);
  const target = targets.find((t) => t.id === draft.spawn_option_id);
  const harnesses = [...new Map(targets.map((t) => [t.harness_id, t.harness_name])).entries()];
  const model = target?.models.find((m) => m.id === draft.model);
  const efforts = model?.efforts ?? target?.efforts ?? [];
  const act = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try { await action(); } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const selectTarget = (id: string) => setDraft({ ...draft, spawn_option_id: id, harness_id: undefined, provider_route_id: undefined, model: null, effort: null, extra_args: null });
  return <form className="launch-config-form" aria-label="Launch Configuration" onSubmit={(e) => { e.preventDefault(); void act(() => onSave(draft)); }}>
    <p>{draft.generated && !draft.generated.user_owned ? 'Generated configuration. Saving an edit preserves your choices against catalogue updates.' : 'User-owned configuration'}</p>
    <fieldset disabled={busy}>
      <label>Name<input autoFocus required value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} /></label>
      <label>Harness<select value={target?.harness_id ?? ''} onChange={(e) => selectTarget(targets.find((t) => t.harness_id === e.target.value)!.id)}>
        {!target && <option value="">Select a harness</option>}
        {harnesses.map(([id, name]) => <option key={id} value={id}>{name}</option>)}
      </select></label>
      <label>Provider<select value={target?.id ?? ''} onChange={(e) => selectTarget(e.target.value)}>
        {!target && <option value="">Select a provider</option>}
        {targets.filter((t) => t.harness_id === target?.harness_id).map((t) => <option key={t.id} value={t.id}>{t.provider_name}</option>)}
      </select></label>
      {target?.supports_model && <label>Model{target.manual_model ?
        <input value={draft.model ?? ''} placeholder="Default" onChange={(e) => setDraft({ ...draft, model: e.target.value || null, effort: null })} /> :
        <select value={draft.model ?? ''} onChange={(e) => setDraft({ ...draft, model: e.target.value || null, effort: null })}>
          <option value="">Default</option>
          {draft.model && !model && <option value={draft.model}>{draft.model} (advanced route)</option>}
          {target.models.map((m) => <option key={m.id} value={m.id}>{m.name} ({m.id})</option>)}
        </select>}
      </label>}
      {efforts.length > 0 && <label>Effort<select value={draft.effort ?? ''} onChange={(e) => setDraft({ ...draft, effort: e.target.value || null })}>
        <option value="">Default</option>{efforts.map((effort) => <option key={effort} value={effort}>{effort}</option>)}
      </select></label>}
      {target?.supports_extra_args && <label>Extra arguments<input value={draft.extra_args ?? ''} onChange={(e) => setDraft({ ...draft, extra_args: e.target.value || null })} /></label>}
      <div className="launch-config-actions">
        <button type="submit" disabled={!target || !draft.name.trim()}>Save</button>
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
