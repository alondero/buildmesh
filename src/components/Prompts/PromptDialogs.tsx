import { useRef, useState } from 'react';
import type { SavedPrompt } from '../../types/generated/SavedPrompt';
import type { Mesh } from '../../types/generated/Mesh';
import type { AgentNode } from '../../types/generated/AgentNode';
import type { SpawnOption } from '../../lib/groups';
import { formatError } from '../../lib/errorUtils';
import { focusNode } from '../../lib/navigation';
import { Modal, ModalCloseButton } from '../shared/Modal';
import { PromptSpawnButton } from './PromptSpawnButton';
import { PromptMeshSelector } from './PromptMeshSelector';
import { useMeshStore } from '../../stores/meshStore';

const fieldClass = 'w-full rounded-md border border-border-default bg-bg-input px-3 py-2 text-sm text-text-primary focus:outline-none focus:border-accent-cyan';
const buttonClass = 'min-h-7 rounded-md text-on-accent-cyan bg-accent-cyan/80 px-3 py-1.5 text-xs font-medium hover:bg-accent-cyan active:bg-accent-blue disabled:bg-bg-card-hover disabled:text-text-muted';

export function PromptEditor({ prompt, mode, onSave, onClose }: {
  prompt: SavedPrompt; mode: 'new' | 'edit' | 'duplicate';
  onSave: (name: string, body: string) => Promise<void>; onClose: () => void;
}) {
  const [name, setName] = useState(prompt.name);
  const [body, setBody] = useState(prompt.body);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const operation = useRef(false);
  const titleRef = useRef<HTMLInputElement>(null);
  const title = mode === 'edit' ? 'Edit saved prompt' : mode === 'duplicate' ? 'Duplicate prompt' : 'New prompt';
  return <Modal onClose={() => { if (!operation.current) onClose(); }} labelledBy="prompt-editor-title" maxWidth="max-w-2xl"
    defaultFocusRef={titleRef} dirty={name !== prompt.name || body !== prompt.body} className="flex max-h-[90vh] flex-col p-4">
    <div className="mb-3 flex shrink-0 items-start justify-between gap-2">
      <h2 id="prompt-editor-title" className="text-sm font-semibold text-text-primary">{title}</h2>
      <ModalCloseButton onClose={onClose} />
    </div>
    <form className="min-h-0 space-y-3 overflow-y-auto overflow-x-hidden" onSubmit={async event => {
      event.preventDefault();
      if (operation.current || !name.trim() || !body.trim()) return;
      operation.current = true; setBusy(true); setError(null);
      try { await onSave(name.trim(), body.trim()); }
      catch (cause) { setError(formatError(cause)); }
      finally { operation.current = false; setBusy(false); }
    }}>
      {error && <p role="alert" className="break-words text-sm text-status-error">{error}</p>}
      <label className="block text-xs text-text-secondary">Title
        <input ref={titleRef} maxLength={120} value={name} disabled={busy} onChange={event => setName(event.target.value)} className={`${fieldClass} mt-1`} />
      </label>
      <label className="block text-xs text-text-secondary">Prompt
        <textarea value={body} disabled={busy} onChange={event => setBody(event.target.value)} className={`${fieldClass} mt-1 h-64 resize-y`} placeholder="What should a fresh agent do?" />
      </label>
      <p className="text-xs text-text-secondary">Available in every Mesh. Text is sent as written.</p>
      <div className="flex flex-wrap justify-end gap-2">
        <button type="submit" disabled={busy || !name.trim() || !body.trim()} className={buttonClass}>{busy ? 'Saving…' : mode === 'edit' ? 'Save changes' : 'Save prompt'}</button>
      </div>
    </form>
  </Modal>;
}

export interface PromptLaunch {
  prompt: SavedPrompt;
  body?: string;
  mesh: Mesh | null;
  node?: AgentNode;
  error?: string;
  providerId?: string;
  configurationId?: string;
  loginId?: string;
}

export function PromptLaunchDialog({ launch, providers, busy, onSpawn, onClose }: {
  launch: PromptLaunch; providers: SpawnOption[]; busy: boolean;
  onSpawn: (mesh: Mesh, body: string, providerId?: string, configurationId?: string, loginId?: string) => void;
  onClose: () => void;
}) {
  const [body, setBody] = useState(launch.body ?? launch.prompt.body);
  const meshes = useMeshStore(state => state.meshes);
  const [meshId, setMeshId] = useState(launch.mesh?.id ?? null);
  // A retry belongs to its already-created node; it cannot be retargeted.
  const mesh = launch.node ? launch.mesh : meshes.find(item => item.id === meshId) ?? null;
  const bodyRef = useRef<HTMLTextAreaElement>(null);
  return <Modal onClose={() => { if (!busy) onClose(); }} labelledBy="prompt-launch-title" maxWidth="max-w-2xl"
    dirty={body !== launch.prompt.body} defaultFocusRef={bodyRef} className="flex max-h-[90vh] flex-col p-4">
    <div className="mb-3 flex shrink-0 items-start justify-between gap-2">
      <h2 id="prompt-launch-title" className="break-words text-sm font-semibold text-text-primary">{launch.node ? 'Retry launch' : launch.error ? 'Could not start agent' : 'Customise for this launch'} · {launch.prompt.name}</h2>
      <ModalCloseButton onClose={onClose} />
    </div>
    <div className="mb-3 flex shrink-0 flex-wrap items-center gap-1 text-xs text-text-secondary">
      {launch.node ? <span className="min-w-0 max-w-full break-words">{mesh?.name} · Existing agent</span> : <>
        <span>Spawn in</span><PromptMeshSelector meshes={meshes} mesh={mesh} disabled={busy} onSelect={selected => setMeshId(selected.id)} />
      </>}
      {mesh && <span className="min-w-0 max-w-full break-words">· {promptLaunchLocation(mesh)}</span>}
    </div>
    {launch.error && <div role="alert" className="mb-3 shrink-0 break-words text-sm text-status-error">
      {launch.error}
      {launch.node && <p className="mt-1 text-xs text-text-secondary">The agent was created. Retry uses the same agent and configuration.{' '}
        <button type="button" className="underline min-h-6" onClick={() => focusNode(launch.node!.id, { view: 'mesh', scope: 'node' })}>View agent</button>
      </p>}
    </div>}
    <div className="min-h-0 space-y-3 overflow-y-auto overflow-x-hidden">
      <label className="block text-xs text-text-secondary">Prompt for this launch
        <textarea ref={bodyRef} value={body} disabled={busy} onChange={event => setBody(event.target.value)} className={`${fieldClass} mt-1 h-64 resize-y`} />
      </label>
      <p className="text-xs text-text-secondary">Your saved prompt stays unchanged. Text is sent as written.</p>
    </div>
    <div className="mt-3 flex shrink-0 justify-end">
      {launch.node ? <button type="button" disabled={busy || !mesh || !body.trim()} className={buttonClass}
        onClick={() => { if (mesh) onSpawn(mesh, body, launch.providerId, launch.configurationId, launch.loginId); }}>{busy ? 'Starting…' : 'Retry launch'}</button>
        : <PromptSpawnButton key={mesh?.id ?? 'none'} meshId={mesh?.id ?? null} promptId={`custom-${launch.prompt.id}`} name={launch.prompt.name}
          providers={providers} busy={busy} disabled={busy || !mesh || !body.trim() || providers.length === 0} portalMenu={false}
          onSpawn={(providerId, configurationId, loginId) => { if (mesh) onSpawn(mesh, body, providerId, configurationId, loginId); }} />}
    </div>
  </Modal>;
}

export function promptLaunchLocation(mesh: Mesh): string {
  return mesh.use_worktree ? `from ${mesh.base_ref || 'origin/main'}` : 'in Mesh folder';
}
