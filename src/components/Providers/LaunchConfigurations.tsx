import { useEffect, useState } from 'react';
import type { SpawnConfiguration } from '../../types/generated/SpawnConfiguration';
import type { LaunchTarget } from '../../types/generated/LaunchTarget';
import { LaunchConfigurationEditor } from './LaunchConfigurationEditor';

export interface LaunchConfigurationApi {
  list: () => Promise<SpawnConfiguration[]>;
  targets: () => Promise<LaunchTarget[]>;
  save: (value: SpawnConfiguration) => Promise<SpawnConfiguration>;
  remove: (id: string) => Promise<void>;
}

export function LaunchConfigurations({ api, onDirtyChange, onChanged, refreshToken }: { api: LaunchConfigurationApi; onDirtyChange?: (dirty: boolean) => void; onChanged?: () => void; refreshToken?: unknown }) {
  const [values, setValues] = useState<SpawnConfiguration[]>([]);
  const [targets, setTargets] = useState<LaunchTarget[]>([]);
  const [draft, setDraft] = useState<SpawnConfiguration | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [loading, setLoading] = useState(true);
  useEffect(() => {
    let current = true;
    Promise.resolve().then(() => Promise.all([api.list(), api.targets()])).then(([list, options]) => {
      if (!Array.isArray(list) || !Array.isArray(options)) throw new Error('Launch Configurations could not be loaded');
      if (current) { setValues(list); setTargets(options); setLoading(false); setError(null); }
    }).catch((e: unknown) => { if (current) { setError(String(e)); setLoading(false); } });
    return () => { current = false; };
  }, [api, revision, refreshToken]);
  const refresh = () => { setDraft(null); setRevision((v) => v + 1); onChanged?.(); };
  if (draft) return <LaunchConfigurationEditor key={draft.id} value={draft} targets={targets} onCancel={() => setDraft(null)}
    onDirtyChange={onDirtyChange}
    onSave={async (value) => { await api.save(value); refresh(); }}
    onDelete={draft.id ? async () => { await api.remove(draft.id); refresh(); } : undefined} />;
  return <div className="launch-config-form">
    {loading && <p role="status">Loading Launch Configurations…</p>}
    {error && <p role="alert">{error} <button onClick={() => setRevision((v) => v + 1)}>Retry</button></p>}
    {values.map((value) => <div key={value.id} className="launch-config-item">
      <p>{value.name} · {value.generated && !value.generated.user_owned ? 'Generated' : 'User-owned'}</p>
      <div className="launch-config-actions">
        <button onClick={() => setDraft(value)} aria-label={`Edit ${value.name}`}>Edit</button>
        <button onClick={() => setDraft({ ...value, id: '', name: `${value.name} copy`, generated: undefined, resolved: undefined })} aria-label={`Clone ${value.name}`}>Clone</button>
      </div>
    </div>)}
    <button disabled={loading || !targets.length} onClick={() => setDraft({ id: '', name: '', spawn_option_id: targets[0].id, model: null, effort: null, extra_args: null })}>New Launch Configuration</button>
  </div>;
}
