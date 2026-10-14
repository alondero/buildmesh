import { useRef, useState } from 'react';
import { issueLabelCatalog, isAuthError, setIssueLabel, type GitHubIssue } from '../api';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import type { IssueLabelCatalog } from '../../types/generated/IssueLabelCatalog';

export function IssueTags({ meshId, issue, onChange, onError, onAuthFailed }: {
  meshId: number;
  issue: GitHubIssue;
  onChange: (label: string, present: boolean) => void;
  onError: (error: string) => void;
  onAuthFailed?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState('');
  const [catalog, setCatalog] = useState<IssueLabelCatalog | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [writeError, setWriteError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  const lifetime = useRef<AbortSignal | null>(null);
  useAsyncEffect(signal => { lifetime.current = signal; }, []);
  useAsyncEffect(signal => {
    if (!open) return;
    setCatalog(null);
    setLoadError(null);
    void issueLabelCatalog(meshId).then(value => {
      if (!signal.aborted) setCatalog(value);
    }).catch(e => {
      if (signal.aborted) return;
      if (isAuthError(e)) onAuthFailed?.();
      else setLoadError((e as Error).message);
    });
  }, [meshId, open, retry, onAuthFailed]);
  const toggle = async (label: string, present: boolean) => {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setWriteError(null);
    try {
      await setIssueLabel(meshId, issue.number, label, present);
      onChange(label, present);
    } catch (e) {
      if (isAuthError(e)) onAuthFailed?.();
      else if (lifetime.current?.aborted) onError((e as Error).message);
      else setWriteError((e as Error).message);
    } finally {
      pending.current = false;
      if (!lifetime.current?.aborted) setBusy(false);
    }
  };
  const watched = (label: string) => catalog?.autopilot_triggers[label.toLowerCase()];
  const choices = [...new Set([...issue.labels, ...(catalog?.labels ?? [])])]
    .filter(label => label.toLowerCase().includes(search.trim().toLowerCase()))
    .sort((a, b) => Number(!!watched(b)) - Number(!!watched(a)) || a.localeCompare(b));
  return <div onClick={e => e.stopPropagation()} style={{ minWidth: 0 }}>
    <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', alignItems: 'center' }}>
      {issue.labels.map(label => <span key={label} style={{ fontSize: 11, color: watched(label) ? 'var(--accent)' : 'var(--text-dim)', background: 'var(--surface-2)', padding: '2px 6px', borderRadius: 4, overflowWrap: 'anywhere', maxWidth: '100%' }}>{label}</span>)}
      <button className="btn-ghost" aria-label={`Edit tags for issue #${issue.number}`} aria-expanded={open} disabled={busy} onClick={() => setOpen(value => !value)}>Edit tags</button>
    </div>
    {open && <div role="group" aria-label={`Tags for issue #${issue.number}`} style={{ marginTop: 8 }}>
      <input className="field" aria-label="Filter tags" placeholder="Filter tags…" value={search} onChange={e => setSearch(e.target.value)} />
      {!catalog && !loadError && <p role="status">Loading tags…</p>}
      {loadError && <p role="alert">{loadError}<button className="btn-ghost" onClick={() => setRetry(value => value + 1)}>Retry loading tags</button></p>}
      {catalog && <div style={{ maxHeight: 240, overflowY: 'auto' }}>
        {choices.map(label => <label key={label} style={{ display: 'flex', gap: 10, padding: '12px 0', overflowWrap: 'anywhere', alignItems: 'start' }}>
          <input type="checkbox" checked={issue.labels.some(value => value.toLowerCase() === label.toLowerCase())} disabled={busy} onChange={e => void toggle(label, e.target.checked)} />
          <span style={{ minWidth: 0 }}>{label}{watched(label) && <small style={{ display: 'block', color: 'var(--accent)' }}>Autopilot: {watched(label)!.join(', ')}. Eligible for pickup on the next poll; dependencies and existing runs still apply.</small>}</span>
        </label>)}
        {choices.length === 0 && <p>{search ? 'No matching tags' : 'No repository labels'}</p>}
      </div>}
      {busy && <p role="status">Saving tags…</p>}
      {writeError && <p role="alert" style={{ color: 'var(--red)' }}>{writeError}</p>}
    </div>}
  </div>;
}
