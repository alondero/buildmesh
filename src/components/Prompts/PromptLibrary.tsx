import { useEffect, useRef, useState } from 'react';
import type { SavedPrompt } from '../../types/generated/SavedPrompt';
import type { SpawnOption } from '../../lib/groups';
import { BUILT_IN_PROMPTS, searchPrompts } from '../../lib/promptLibrary';
import { deleteSavedPrompt, listSavedPrompts, savePrompt } from '../../lib/tauri/preferences';
import { getDefaultProvider } from '../../lib/tauri/provider';
import { formatError } from '../../lib/errorUtils';
import { SpawnButtonCluster } from '../Sidebar/SpawnButtonCluster';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';

const EMPTY: SavedPrompt = { id: '', name: '', body: '' };
const fieldClass = 'w-full rounded-md border border-border-default bg-bg-input px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-accent-cyan';
const buttonClass = 'min-h-6 rounded-md border border-border-default px-2 py-1 text-xs text-text-secondary hover:bg-bg-card-hover disabled:opacity-40';

export function PromptLibrary({ meshId, providers, initialPrompt = '', retryingNode = false, onSpawn, onDirtyChange }: {
  meshId: number;
  providers: SpawnOption[];
  initialPrompt?: string;
  retryingNode?: boolean;
  onSpawn: (body: string, providerId: string, altKey: boolean, configurationId?: string, loginId?: string) => Promise<void>;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const initial = { ...EMPTY, body: initialPrompt };
  const [saved, setSaved] = useState<SavedPrompt[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [loadAttempt, setLoadAttempt] = useState(0);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [baseline, setBaseline] = useState(initial);
  const [draft, setDraft] = useState(initial);
  const [pending, setPending] = useState<SavedPrompt | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState(false);
  const [launching, setLaunching] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const operation = useRef(false);
  const dirty = draft.name !== baseline.name || draft.body !== baseline.body;
  const custom = saved.some(prompt => prompt.id === draft.id);
  const canSave = loaded && draft.name.trim() !== '' && draft.body.trim() !== '' && (!custom || dirty);

  useEffect(() => { onDirtyChange(dirty); }, [dirty, onDirtyChange]);
  useEffect(() => {
    let current = true;
    setLoadError(null);
    listSavedPrompts().then(prompts => {
      if (current) { setSaved(prompts); setLoaded(true); }
    }).catch(err => { if (current) setLoadError(formatError(err)); });
    return () => { current = false; };
  }, [loadAttempt]);

  const adopt = (prompt: SavedPrompt) => {
    setBaseline(prompt); setDraft(prompt); setPending(null); setConfirmDelete(false); setError(null);
  };
  const choose = (prompt: SavedPrompt) => {
    if (dirty) setPending(prompt);
    else adopt(prompt);
  };
  const run = async (action: () => Promise<void>, launch = false) => {
    if (operation.current) return;
    operation.current = true;
    setBusy(true); setMenuOpen(false); setError(null);
    setLaunching(launch);
    try { await action(); }
    catch (err) { setError(formatError(err)); }
    finally { operation.current = false; setBusy(false); setLaunching(false); }
  };
  const save = () => run(async () => {
    const prompt = await savePrompt(custom ? draft.id : null, draft.name, draft.body);
    setSaved(prompts => custom ? prompts.map(item => item.id === prompt.id ? prompt : item) : [...prompts, prompt]);
    adopt(prompt);
  });
  const remove = () => run(async () => {
    setConfirmDelete(false);
    await deleteSavedPrompt(draft.id);
    setSaved(prompts => prompts.filter(item => item.id !== draft.id));
    adopt(EMPTY);
  });
  const spawn = (providerId: string, altKey: boolean, configurationId?: string, loginId?: string) =>
    run(() => onSpawn(draft.body.trim(), providerId, altKey, configurationId, loginId), true);
  const promptProviders = providers.filter(option => option.capabilities?.accepts_initial_prompt ?? option.harness_id !== 'terminal');

  return (
    <div data-testid="prompt-library" className="min-w-0 space-y-3" onKeyDown={event => {
      if (event.key !== 'Escape' || !menuOpen || event.nativeEvent.isComposing) return;
      // Own Escape before document listeners can dismiss the enclosing modal.
      event.preventDefault();
      event.stopPropagation();
      setMenuOpen(false);
      event.currentTarget.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]')?.focus();
    }}>
      {loadError && <div role="alert" className="text-sm text-status-error break-words">Could not load saved prompts: {loadError}{' '}
        <button type="button" className="underline min-h-6" onClick={() => setLoadAttempt(value => value + 1)}>Retry</button>
      </div>}
      {error && <p role="alert" className="text-sm text-status-error break-words">{error}</p>}
      <div className="grid min-w-0 gap-4 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.5fr)]">
        <div className="min-w-0 space-y-2">
          <div className="flex gap-2">
            <input aria-label="Search prompts" type="search" placeholder="Search prompts…" value={query} onChange={event => setQuery(event.target.value)} className={`${fieldClass} min-w-0`} />
            <button type="button" disabled={busy} onClick={() => choose(EMPTY)} className={`${buttonClass} shrink-0`}>New prompt</button>
          </div>
          <div className="max-h-44 sm:max-h-80 overflow-y-auto overflow-x-hidden space-y-3" aria-label="Prompt library">
            {!loaded && !loadError && <p role="status" className="text-xs text-text-secondary">Loading saved prompts…</p>}
            {[{ title: 'Your prompts', prompts: saved }, { title: 'Built-in prompts', prompts: BUILT_IN_PROMPTS }].map(group => (
              <div key={group.title}>
                <h3 className="text-2xs font-medium text-text-secondary mb-1">{group.title}</h3>
                {group.title === 'Your prompts' && loaded && saved.length === 0 && <p className="text-xs text-text-muted">Save a prompt to reuse it in any project.</p>}
                {searchPrompts(group.prompts, query).map(prompt => (
                  <button key={prompt.id} type="button" aria-pressed={draft.id === prompt.id} disabled={busy}
                    onClick={() => choose(prompt)} className={`block w-full text-left rounded-md px-2 py-2 text-sm break-words disabled:opacity-40 ${draft.id === prompt.id ? 'bg-bg-selection text-text-primary' : 'text-text-secondary hover:bg-bg-card-hover'}`}>
                    {prompt.name}
                  </button>
                ))}
              </div>
            ))}
            {searchPrompts([...saved, ...BUILT_IN_PROMPTS], query).length === 0 && <p className="text-sm text-text-secondary">No prompts match your search.</p>}
          </div>
        </div>
        <div className="min-w-0 space-y-2">
          {pending && <div role="alert" className="rounded-md border border-border-default p-2 text-xs text-text-secondary">
            Discard your unsaved prompt changes?
            <div className="flex flex-wrap gap-2 mt-2">
              <button type="button" className={buttonClass} onClick={() => setPending(null)}>Keep editing</button>
              <button type="button" className={buttonClass} onClick={() => adopt(pending)}>Discard changes</button>
            </div>
          </div>}
          <label className="block text-xs text-text-secondary">Prompt name
            <input maxLength={120} value={draft.name} disabled={busy} onChange={event => setDraft({ ...draft, name: event.target.value })} className={`${fieldClass} mt-1`} />
          </label>
          <label className="block text-xs text-text-secondary">Initial prompt
            <textarea value={draft.body} disabled={busy} onChange={event => setDraft({ ...draft, body: event.target.value })} placeholder="What should the agent do?" className={`${fieldClass} mt-1 h-44 resize-y`} />
          </label>
          <p className="text-2xs text-text-secondary">Edit for this launch, or save to reuse. Text is sent as written.</p>
          <div className="flex flex-wrap items-center gap-2">
            <button type="button" disabled={!canSave || busy} onClick={save} className={buttonClass}>{custom ? 'Save changes' : draft.id ? 'Save a copy' : 'Save prompt'}</button>
            {dirty && <button type="button" disabled={busy} onClick={() => adopt(baseline)} className={buttonClass}>Reset changes</button>}
            {custom && <button type="button" disabled={busy} onClick={() => setConfirmDelete(true)} className={`${buttonClass} text-status-error`}>Delete prompt</button>}
          </div>
          {confirmDelete && <ConfirmDialog title="Delete prompt" message={`Delete “${baseline.name}” from your library?`}
            confirmLabel="Confirm delete" onCancel={() => setConfirmDelete(false)} onConfirm={remove} />}
          <div className="flex flex-wrap justify-end pt-2">
            {retryingNode ? <button type="button" disabled={busy || !draft.body.trim()} className={buttonClass}
              onClick={() => { void spawn('', false); }}>Retry launch</button> :
            <SpawnButtonCluster providers={promptProviders} dropdownKey={`prompt-${meshId}`} primaryLabel="Spawn" primaryAriaLabel="Spawn with prompt"
              isOpen={menuOpen} onToggleDropdown={() => setMenuOpen(value => !value)} disabled={busy || !draft.body.trim() || promptProviders.length === 0}
              isSpawning={launching} getDefaultProvider={() => getDefaultProvider(meshId)} onSelectProvider={spawn}
              onSpawnDefault={altKey => { void run(async () => {
                await onSpawn(draft.body.trim(), await getDefaultProvider(meshId), altKey);
              }, true); }} />}
          </div>
          {retryingNode && <p className="text-xs text-text-secondary">The agent node was created. Retry sends this prompt to the same node.</p>}
          {promptProviders.length === 0 && <p className="text-xs text-text-secondary">Install an agent CLI or add a provider in Settings to spawn with a prompt.</p>}
        </div>
      </div>
    </div>
  );
}
