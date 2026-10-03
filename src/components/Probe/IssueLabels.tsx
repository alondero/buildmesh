import { useRef, useState } from 'react';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useEscapeKey } from '../../hooks/useEscapeKey';
import { formatError } from '../../lib/errorUtils';
import { getRepoLabels, setIssueLabel, type GitHubIssue } from '../../lib/tauri';

export function IssueLabels({ meshId, issue, triggers, onChange, onError }: {
  meshId: number;
  issue: GitHubIssue;
  triggers: Map<string, string[]>;
  onChange: (label: string, present: boolean) => void;
  onError: (error: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [labels, setLabels] = useState<string[]>([]);
  const [search, setSearch] = useState('');
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [writeError, setWriteError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const lifetime = useRef<AbortSignal | null>(null);
  const pending = useRef(false);
  const editorId = `issue-tags-${meshId}-${issue.number}`;
  const watched = (label: string) => triggers.get(label.toLowerCase());
  const tooltip = (label: string) => {
    const names = watched(label);
    return names ? `Watched by enabled Autopilot Circuit: ${names.join(', ')}. Eligible for pickup on the next poll; dependencies and existing runs still apply.` : label;
  };

  useAsyncEffect(signal => { lifetime.current = signal; }, []);
  useAsyncEffect(signal => {
    if (!open) return;
    setLoading(true);
    setLoadError(null);
    void getRepoLabels(meshId).then(result => {
      if (!signal.aborted) setLabels(result);
    }).catch(err => {
      if (!signal.aborted) setLoadError(formatError(err));
    }).finally(() => {
      if (!signal.aborted) setLoading(false);
    });
  }, [meshId, open, retry]);
  useClickOutside(open ? editorId : null, () => setOpen(false));
  useEscapeKey(() => {
    setOpen(false);
    triggerRef.current?.focus();
  }, open);

  const toggle = async (label: string, present: boolean) => {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setWriteError(null);
    try {
      await setIssueLabel(meshId, issue.number, label, present);
      // The tab owns reconciliation even if its filter unmounts this row.
      onChange(label, present);
    } catch (err) {
      const error = formatError(err);
      if (!lifetime.current?.aborted) setWriteError(error);
      else onError(error);
    } finally {
      pending.current = false;
      if (!lifetime.current?.aborted) setBusy(false);
    }
  };

  // Keep watched labels visible even when the issue has many ordinary tags.
  const watchedLabels = issue.labels.filter(label => watched(label));
  const ordered = [...watchedLabels, ...issue.labels.filter(label => !watched(label))];
  const visible = ordered.slice(0, Math.max(3, watchedLabels.length));
  const choices = [...new Set([...issue.labels, ...labels])]
    .filter(label => label.toLowerCase().includes(search.trim().toLowerCase()))
    .sort((a, b) => Number(!!watched(b)) - Number(!!watched(a)) || a.localeCompare(b));

  return (
    <div className="flex flex-wrap items-center gap-1 min-w-0 max-w-full" data-dropdown-for={editorId}>
      {visible.map(label => (
        <span key={label} data-issue-label={label} data-circuit-trigger-label={watched(label) ? label : undefined}
          title={tooltip(label)}
          className={`max-w-full break-all rounded-md border px-1.5 py-px text-2xs ${watched(label) ? 'border-accent-violet/40 bg-accent-violet/10 text-accent-violet font-medium' : 'border-border-subtle bg-bg-card text-text-secondary'}`}>
          {watched(label) && <span aria-hidden="true">⚡ </span>}{label}
        </span>
      ))}
      {ordered.length > visible.length && (
        <button type="button" title={ordered.slice(visible.length).join(', ')} onClick={() => setOpen(true)}
          className="rounded-md px-1.5 py-px text-2xs text-text-secondary hover:text-text-primary">
          +{ordered.length - visible.length}
        </button>
      )}
      <button type="button" ref={triggerRef} aria-label={`Edit tags for issue #${issue.number}`} title="Edit tags"
        aria-expanded={open} aria-controls={editorId} onClick={() => setOpen(value => !value)}
        className="inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-sm text-text-secondary hover:text-accent-cyan focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent-cyan">
        <svg aria-hidden="true" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8">
          <path d="M20 13l-7 7a2 2 0 0 1-3 0l-8-8V2h10l8 8a2 2 0 0 1 0 3Z" /><circle cx="7" cy="7" r="1" />
        </svg>
      </button>
      {open && (
        <div id={editorId} role="group" aria-label={`Tags for issue #${issue.number}`} aria-busy={busy || loading}
          className="basis-full min-w-0 rounded-md border border-border-default bg-bg-input p-2 space-y-2">
          <input type="text" aria-label="Filter tags" placeholder="Filter tags…" value={search} onChange={event => setSearch(event.target.value)}
            className="w-full min-w-0 rounded-sm border border-border-default bg-bg-input px-2 py-1 text-xs text-text-primary focus:outline-none focus:border-accent-cyan" />
          {loading && <p role="status" className="text-2xs text-text-secondary">Loading tags…</p>}
          {loadError && <div role="alert" className="text-2xs text-accent-red break-words">
            {loadError}<button type="button" onClick={() => setRetry(value => value + 1)} className="block text-accent-cyan">Retry loading tags</button>
          </div>}
          {!loading && !loadError && <div className="max-h-48 overflow-y-auto overflow-x-hidden space-y-1">
            {choices.map(label => (
              <label key={label} title={tooltip(label)} className={`flex items-start gap-2 text-xs break-all ${watched(label) ? 'text-accent-violet' : 'text-text-secondary'}`}>
                <input type="checkbox" checked={issue.labels.some(value => value.toLowerCase() === label.toLowerCase())} disabled={busy}
                  onChange={event => void toggle(label, event.target.checked)} className="mt-0.5 shrink-0 accent-accent-cyan disabled:opacity-40" />
                <span>{label}{watched(label) && <span className="block text-2xs">Autopilot</span>}</span>
              </label>
            ))}
            {choices.length === 0 && <p className="text-2xs text-text-secondary">{search ? 'No matching tags' : 'No repository labels'}</p>}
          </div>}
          {busy && <p role="status" className="text-2xs text-text-secondary">Saving tags…</p>}
        </div>
      )}
      {writeError && <p role="alert" className="basis-full text-2xs text-accent-red break-words">{writeError}</p>}
    </div>
  );
}
