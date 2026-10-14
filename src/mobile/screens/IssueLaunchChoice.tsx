import { useState } from 'react';
import { listProviders, isAuthError, type Provider } from '../api';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { Sheet } from '../ui';

export function IssueLaunchChoice({ busy, error, onCancel, onStart, onAuthFailed }: {
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onStart: (provider?: string) => void;
  onAuthFailed?: () => void;
}) {
  const [options, setOptions] = useState<Provider[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const [choice, setChoice] = useState('');
  useAsyncEffect(signal => {
    setOptions(null);
    setLoadError(null);
    void listProviders().then(rows => {
      if (!signal.aborted) setOptions(rows.filter(row => !row.unavailable_reason && !row.configuration && row.capabilities.supports_prefill));
    }).catch(e => {
      if (signal.aborted) return;
      if (isAuthError(e)) onAuthFailed?.();
      else setLoadError((e as Error).message);
    });
  }, [retry, onAuthFailed]);
  return <Sheet label="Choose a launch configuration" testId="issue-launch-choice" dismissible={!busy} onClose={onCancel}>
    <h3>Choose a launch configuration</h3>
    <label>Launch configuration
      <select className="field" value={choice} disabled={busy || options === null} onChange={e => setChoice(e.target.value)}>
        <option value="">Mesh default</option>
        {options?.map(row => <option key={row.id} value={row.id}>{row.label}</option>)}
      </select>
    </label>
    {options === null && !loadError && <p role="status">Loading launch configurations…</p>}
    {loadError && <p role="alert">{loadError}<button className="btn-ghost" onClick={() => setRetry(value => value + 1)}>Retry</button></p>}
    {error && <p role="alert" style={{ color: 'var(--red)' }}>{error}</p>}
    <div style={{ display: 'flex', gap: 8, marginTop: 16 }}>
      <button className="btn-ghost" disabled={busy} onClick={onCancel}>Cancel</button>
      <button className="btn-primary" disabled={busy || options === null} onClick={() => onStart(choice || undefined)}>{busy ? 'Starting…' : 'Start'}</button>
    </div>
  </Sheet>;
}
