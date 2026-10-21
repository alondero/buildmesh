import { lazy, Suspense, useEffect, useRef, useState } from 'react';
import { companionRunning, useCompanionStore } from '../../stores/companionStore';

const CompanionResponse = lazy(() => import('./CompanionResponse'));

export function CompanionMode({ initialPrompt, onBack }: { initialPrompt: string; onBack: () => void }) {
  const task = useCompanionStore(s => s.task);
  const starting = useCompanionStore(s => s.starting);
  const error = useCompanionStore(s => s.error);
  const execute = useCompanionStore(s => s.execute);
  const cancel = useCompanionStore(s => s.cancel);
  const respond = useCompanionStore(s => s.respond);
  const [prompt, setPrompt] = useState(initialPrompt);
  const input = useRef<HTMLInputElement>(null);
  const confirm = useRef<HTMLButtonElement>(null);
  useEffect(() => { if (task?.confirmation) confirm.current?.focus(); }, [task?.confirmation]);
  const running = starting || companionRunning(task);
  useEffect(() => { input.current?.focus(); }, []);

  return <div className="border-t-2 border-accent-violet text-text-primary" data-testid="companion-mode">
    <div className="flex items-center gap-3 p-4">
      <span className="text-accent-violet" aria-hidden>✦</span>
      <input ref={input} aria-label="Ask Companion" value={prompt} disabled={running}
        placeholder="Ask the Buildmesh Companion to inspect, spawn, or drive nodes..."
        className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-text-muted"
        onChange={event => setPrompt(event.target.value)}
        onKeyDown={event => {
          if (event.key === 'Enter' && prompt.trim() && !running) { event.preventDefault(); void execute(prompt); }
        }} />
      {!running && <button type="button" disabled={!prompt.trim()} className="text-accent-violet disabled:opacity-50" onClick={() => void execute(prompt)}>Ask</button>}
    </div>
    <div className="max-h-[55vh] overflow-y-auto p-4 border-t border-border-subtle space-y-3 break-words">
      {starting && <p role="status">Starting Companion…</p>}
      {task && <>
        <p className="text-sm text-text-secondary">{task.prompt}</p>
        <ol aria-label="Companion progress" className="space-y-1 text-sm text-text-secondary">
          {task.steps.map((step, index) => <li key={index}>{step}</li>)}
        </ol>
        <p role="status" className="text-accent-violet text-sm">{task.status}</p>
        {task.confirmation && <div className="rounded-md border border-accent-amber p-3 space-y-3" role="group" aria-label="Confirm Companion action">
          <p className="whitespace-pre-wrap">{task.confirmation}</p>
          <div className="flex gap-4">
            <button ref={confirm} type="button" className="text-accent-violet" onClick={() => void respond(true)}>Confirm execution (Enter)</button>
            <button type="button" onClick={() => void respond(false)}>Cancel action (Esc)</button>
          </div>
        </div>}
        {task.response && <div className="text-sm space-y-2 [&_table]:w-full [&_td]:p-1 [&_th]:text-left [&_pre]:whitespace-pre-wrap">
          <Suspense fallback={<p>{task.response}</p>}><CompanionResponse response={task.response} /></Suspense>
        </div>}
        {task.error && <p role="alert" className="text-accent-red">{task.error}</p>}
      </>}
      {error && <p role="alert" className="text-accent-red">{error}</p>}
    </div>
    <div className="flex justify-between p-3 border-t border-border-subtle text-sm">
      <button type="button" onClick={onBack}>Back to commands</button>
      {running && <button type="button" className="text-accent-red" onClick={() => void cancel()}>Cancel task</button>}
    </div>
  </div>;
}
