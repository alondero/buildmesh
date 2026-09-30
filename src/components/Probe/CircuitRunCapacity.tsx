import { useEffect, useRef, useState } from 'react';
import { formatError } from '../../lib/errorUtils';
import { updateMeshCircuitRunCapacity } from '../../lib/tauri';
import { useMeshStore } from '../../stores/meshStore';

/** Mount with a mesh-id key so an old save cannot update the next mesh's controls. */
export function CircuitRunCapacity({ meshId, capacity }: { meshId: number; capacity: number }) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(false);
  const saving = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  async function save(value: number) {
    if (saving.current) return;
    saving.current = true;
    setPending(true);
    setError(null);
    try {
      await updateMeshCircuitRunCapacity(meshId, value);
      await useMeshStore.getState().refreshMeshes();
    } catch (cause) {
      if (mounted.current) setError(formatError(cause));
    } finally {
      saving.current = false;
      if (mounted.current) setPending(false);
    }
  }

  return <>
    <label className="block text-xs text-text-secondary mb-2">
      Max concurrent circuit runs
      <select aria-label="Max concurrent circuit runs" value={capacity} disabled={pending}
        className="mt-1 block w-full rounded-sm border border-border-subtle bg-bg-card p-1"
        onChange={(event) => void save(Number(event.target.value))}>
        {[1, 2, 3, 4, 5, 6, 7, 8].map(value => <option key={value} value={value}>{value}</option>)}
      </select>
    </label>
    {error && <p role="alert" className="text-xs text-status-error mb-2">{error}</p>}
  </>;
}
