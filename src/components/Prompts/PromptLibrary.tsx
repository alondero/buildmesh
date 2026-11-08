import { useEffect, useRef, useState } from 'react';
import type { SavedPrompt } from '../../types/generated/SavedPrompt';
import type { Mesh } from '../../types/generated/Mesh';
import type { AgentNode } from '../../types/generated/AgentNode';
import type { SpawnOption } from '../../lib/groups';
import { BUILT_IN_PROMPTS, searchPrompts } from '../../lib/promptLibrary';
import { deleteSavedPrompt, listSavedPrompts, savePrompt } from '../../lib/tauri/preferences';
import { getDefaultProvider } from '../../lib/tauri/provider';
import { useProbeResource, type ProbeLoader } from '../../lib/probeResource';
import { enterMesh, focusNode } from '../../lib/navigation';
import { formatError } from '../../lib/errorUtils';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { ProbeToolbar } from '../Probe/ProbeToolbar';
import { ProbeTabBody } from '../Probe/ProbeTabBody';
import { ProbeRow } from '../Probe/ProbeRow';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { PromptActions } from './PromptActions';
import { PromptEditor, PromptLaunchDialog, promptLaunchLocation, type PromptLaunch } from './PromptDialogs';
import { PromptSpawnButton } from './PromptSpawnButton';

export const loadPrompts: ProbeLoader<SavedPrompt[]> = () => listSavedPrompts();
const EMPTY: SavedPrompt = { id: '', name: '', body: '' };
const buttonClass = 'min-h-6 rounded-md border border-border-default px-2 py-1 text-xs text-text-secondary hover:bg-bg-card-hover disabled:opacity-40';
interface Editor { prompt: SavedPrompt; mode: 'new' | 'edit' | 'duplicate' }

export function PromptLibrary({ mesh, providers }: { mesh: Mesh | null; providers: SpawnOption[] }) {
  // Content is Host-owned; changing launch context must not refetch over edits.
  const resource = useProbeResource(loadPrompts, { meshId: null }, 'prompt-library');
  const { data: saved, error: loadError, loading } = resource.state;
  const meshes = useMeshStore(state => state.meshes);
  const [query, setQuery] = useState('');
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [editor, setEditor] = useState<Editor | null>(null);
  const [deleting, setDeleting] = useState<SavedPrompt | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [launch, setLaunch] = useState<PromptLaunch | null>(null);
  const [spawningId, setSpawningId] = useState<string | null>(null);
  const [success, setSuccess] = useState<{ node: AgentNode; meshName: string } | null>(null);
  const [deletingBusy, setDeletingBusy] = useState(false);
  const operation = useRef(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  const promptProviders = providers.filter(option => option.capabilities?.accepts_initial_prompt ?? option.harness_id !== 'terminal');
  const busy = spawningId !== null || deletingBusy;

  const spawn = async (request: PromptLaunch) => {
    if (operation.current) return;
    operation.current = true;
    setSpawningId(request.prompt.id); setError(null); setSuccess(null);
    let node = request.node;
    let providerId = request.providerId;
    try {
      const body = (request.body ?? request.prompt.body).trim();
      if (!body) throw new Error('Enter a prompt before spawning.');
      if (node) {
        const current = useAgentNodeStore.getState().nodesById[node.id];
        if (!current || current.status === 'archived') throw new Error('This agent is no longer available. Close this dialog to start a fresh agent.');
        if (!['idle', 'error', 'suspended'].includes(current.status) || current.provider !== node.provider) {
          throw new Error('This agent has changed since the failed launch. View the agent before retrying.');
        }
      }
      if (!node) {
        providerId ??= await getDefaultProvider(request.mesh.id);
        if (!promptProviders.some(option => option.id === providerId)) {
          throw new Error('Choose an agent that supports an initial prompt from the arrow menu.');
        }
        if (!useMeshStore.getState().meshesById.has(request.mesh.id)) throw new Error('This Mesh is no longer available.');
        // A fresh create uses the Mesh defaults. Never read the focused agent,
        // inherit its checkout, or let a delayed completion change selection.
        node = await useAgentNodeStore.getState().createAgentNode(request.mesh.id, request.mesh.name,
          request.mesh.path, 'main', providerId, request.mesh.use_worktree, request.configurationId, body, request.loginId);
      }
      await useAgentNodeStore.getState().spawnAgent(node.id, node.provider, { prefill: body });
      if (mounted.current) { setLaunch(null); setSuccess({ node, meshName: request.mesh.name }); }
    } catch (cause) {
      if (mounted.current) setLaunch({ ...request, providerId, node, error: formatError(cause) });
    } finally {
      operation.current = false;
      if (mounted.current) setSpawningId(null);
    }
  };

  const save = async (name: string, body: string) => {
    if (!editor || saved === null) throw new Error('Load your library before saving a prompt.');
    const prompt = await savePrompt(editor.mode === 'edit' ? editor.prompt.id : null, name, body);
    resource.mutate(prompts => editor.mode === 'edit'
      ? prompts.map(item => item.id === prompt.id ? prompt : item) : [...prompts, prompt]);
    if (mounted.current) {
      setQuery(''); setExpanded(current => ({ ...current, [prompt.id]: true })); setEditor(null);
      requestAnimationFrame(() => {
        if (mounted.current) document.querySelector<HTMLElement>(`[data-prompt-row="${prompt.id}"] [aria-expanded]`)?.focus();
      });
    }
  };

  const remove = async () => {
    if (!deleting || operation.current) return;
    operation.current = true; setDeletingBusy(true); setDeleting(null); setError(null);
    try {
      await deleteSavedPrompt(deleting.id);
      resource.mutate(prompts => prompts.filter(item => item.id !== deleting.id));
      searchRef.current?.focus();
    } catch (cause) { setError(formatError(cause)); }
    finally { operation.current = false; setDeletingBusy(false); }
  };

  const groups = [{ title: 'Your prompts', prompts: saved ?? [] }, { title: 'Built-in prompts', prompts: BUILT_IN_PROMPTS }];
  const matches = groups.map(group => ({ ...group, prompts: searchPrompts(group.prompts, query) }));
  return <div data-testid="prompt-library" className="flex h-full min-h-0 min-w-0 flex-col">
    <ProbeToolbar className="shrink-0" trailing={<button type="button" className={buttonClass} disabled={busy || saved === null}
      onClick={() => setEditor({ prompt: EMPTY, mode: 'new' })}>New prompt</button>}>
      <input ref={searchRef} aria-label="Search prompts" type="search" placeholder="Search prompts…" value={query} onChange={event => setQuery(event.target.value)}
        className="min-h-7 w-full min-w-0 rounded-md border border-border-default bg-bg-input px-2 text-xs text-text-primary focus:outline-none focus:border-accent-cyan" />
    </ProbeToolbar>
    <div className="shrink-0 space-y-1 border-b border-border-subtle px-3 py-2 text-xs text-text-secondary">
      {mesh ? <p data-testid="prompt-launch-context" className="break-words">Fresh spawn · {promptLaunchLocation(mesh)}</p>
        : <label className="block">Select a Mesh to spawn
          <select aria-label="Spawn target Mesh" value="" onChange={event => { if (event.target.value) enterMesh(Number(event.target.value)); }}
            className="mt-1 min-h-7 w-full min-w-0 rounded-md border border-border-default bg-bg-input px-2 text-xs text-text-primary">
            <option value="">Select a Mesh…</option>
            {meshes.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}
          </select>
        </label>}
      {!mesh && <p>Your library is available in every Mesh.</p>}
      {mesh && promptProviders.length === 0 && <p>Install an agent CLI or add a provider in Settings to spawn.</p>}
    </div>
    {loadError && <div role="alert" className="shrink-0 break-words px-3 py-2 text-xs text-status-error">Could not load saved prompts: {loadError}{' '}
      <button type="button" className="min-h-6 underline" onClick={() => void resource.reload()}>Retry</button>
    </div>}
    {error && <p role="alert" className="shrink-0 break-words px-3 py-2 text-xs text-status-error">{error}</p>}
    {success && <div role="status" className="shrink-0 break-words px-3 py-2 text-xs text-text-secondary">Started {success.node.name} in {success.meshName}.{' '}
      <button type="button" className="min-h-6 underline" onClick={() => focusNode(success.node.id, { view: 'mesh', scope: 'node' })}>View agent</button>
    </div>}
    <ProbeTabBody data-testid="prompt-library-body" className="space-y-3">
      {loading && <p role="status" className="text-xs text-text-secondary">Loading saved prompts…</p>}
      {!loading && saved?.length === 0 && !query && <p className="text-xs text-text-secondary">Save your own with New prompt, or try a built-in below.</p>}
      {matches.filter(group => group.prompts.length > 0).map(group => <section key={group.title} aria-label={group.title}>
        <h3 className="mb-1 text-xs font-medium text-text-secondary">{group.title}</h3>
        <div className="space-y-2">{group.prompts.map(prompt => {
          const custom = saved?.some(item => item.id === prompt.id) ?? false;
          return <ProbeRow key={prompt.id} dataAttr="prompt" rowKey={prompt.id} title={prompt.name} url="" iconAriaLabel=""
            isExpanded={expanded[prompt.id] ?? false} onToggle={() => setExpanded(current => ({ ...current, [prompt.id]: !current[prompt.id] }))}
            body={prompt.body} bodyFormat="text" rightSlot={<div className="flex max-w-full items-center gap-1">
              <PromptSpawnButton key={`${mesh?.id ?? 'none'}-${prompt.id}`} meshId={mesh?.id ?? null} promptId={prompt.id} name={prompt.name}
                providers={promptProviders} busy={spawningId === prompt.id} disabled={busy || !mesh || promptProviders.length === 0}
                onSpawn={(providerId, configurationId, loginId) => { if (mesh) void spawn({ prompt, mesh, providerId, configurationId, loginId }); }} />
              <PromptActions name={prompt.name} custom={custom} disabled={busy || saved === null}
                onEdit={() => setEditor({ prompt, mode: 'edit' })}
                onDuplicate={() => setEditor({ prompt: { ...prompt, name: `${prompt.name.slice(0, 113)} (copy)` }, mode: 'duplicate' })}
                onDelete={() => setDeleting(prompt)} />
            </div>} belowSlot={expanded[prompt.id] && <button type="button" className={`${buttonClass} mb-1`} disabled={busy || !mesh}
              onClick={() => { if (mesh) setLaunch({ prompt, mesh }); }}>Customise for this launch</button>} />;
        })}</div>
      </section>)}
      {matches.every(group => group.prompts.length === 0) && !loading && <p className="text-sm text-text-secondary">No prompts match your search.</p>}
    </ProbeTabBody>
    {editor && <PromptEditor prompt={editor.prompt} mode={editor.mode} onSave={save} onClose={() => setEditor(null)} />}
    {deleting && <ConfirmDialog title="Delete prompt" message={`Delete “${deleting.name}” from your library?`} confirmLabel="Confirm delete"
      onCancel={() => { if (!deletingBusy) setDeleting(null); }} onConfirm={remove} />}
    {launch && <PromptLaunchDialog launch={launch} providers={promptProviders} busy={spawningId !== null} onClose={() => setLaunch(null)}
      onSpawn={(body, providerId, configurationId, loginId) => void spawn({ ...launch, body, providerId, configurationId, loginId })} />}
  </div>;
}
