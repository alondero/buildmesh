import { useState } from 'react';
import { circuitPromptPayload } from '../../lib/tauri/circuitEvidence';
import type { CircuitPromptRequest } from '../../types/generated/CircuitPromptRequest';

export function CopyPromptPayload({ request, disabled }: { request: CircuitPromptRequest; disabled: boolean }) {
  const [state, setState] = useState<'idle' | 'copying' | 'copied'>('idle');
  const [error, setError] = useState<string | null>(null);

  async function copy() {
    setState('copying');
    setError(null);
    try {
      const text = await circuitPromptPayload(request);
      await navigator.clipboard.writeText(text);
      setState('copied');
    } catch (cause) {
      setError(String(cause));
      setState('idle');
    }
  }

  return <div className="min-w-0">
    <button type="button" disabled={disabled || state === 'copying'} onClick={() => void copy()}
      className="min-h-6 rounded-md border border-border-subtle px-1.5 py-1 text-text-primary hover:bg-bg-card-hover disabled:opacity-40">
      {state === 'copying' ? 'Copying…' : 'Copy payload'}
    </button>
    {state === 'copied' && <span role="status" className="ml-1.5 text-status-success">Copied</span>}
    {error !== null && <p role="alert" className="mt-1 break-words text-status-error">Could not copy payload: {error}</p>}
  </div>;
}
