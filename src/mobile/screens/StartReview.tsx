import { useEffect, useRef, useState } from 'react';
import { isAuthError, listNodeReviews, listProviders, startNodeReview, type Provider } from '../api';
import type { NodeReviewOption } from '../../types/generated/NodeReviewOption';
import { blocksReviewCircuit } from '../../components/Circuits/harnessCapabilities';
import { groupByHarness } from '../../lib/groups';
import { Sheet } from '../ui';

export function StartReview({ nodeId, onAuthFailed, onBusyChange }: { nodeId: number; onAuthFailed: () => void; onBusyChange?: (busy: boolean) => void }) {
  const [open, setOpen] = useState(false);
  const [options, setOptions] = useState<NodeReviewOption[] | null>(null);
  const [choice, setChoice] = useState('');
  const [rounds, setRounds] = useState(3);
  const [reviewer, setReviewer] = useState('');
  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [providerError, setProviderError] = useState('');
  const [menuRetry, setMenuRetry] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const active = useRef(true);
  const sending = useRef(false);
  useEffect(() => {
    active.current = true;
    return () => { active.current = false; onBusyChange?.(false); };
  }, [onBusyChange]);
  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    setOptions(null);
    setError('');
    listNodeReviews(nodeId, controller.signal).then((rows) => {
      if (!controller.signal.aborted) {
        setOptions(rows);
        setChoice((current) => rows.some((row) => String(row.circuit_id ?? '') === current) ? current : '');
      }
    }).catch((e: unknown) => {
      if (controller.signal.aborted) return;
      if (isAuthError(e)) onAuthFailed();
      else setError((e as Error).message);
    });
    return () => controller.abort();
  }, [open, nodeId, onAuthFailed]);

  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    setProviders(null);
    setProviderError('');
    listProviders(controller.signal).then((rows) => {
      if (controller.signal.aborted) return;
      if (!Array.isArray(rows)) throw new Error('Launch configurations are unavailable. Update Buildmesh and retry.');
      setProviders(rows);
    }).catch((e: unknown) => {
      if (!controller.signal.aborted) setProviderError((e as Error).message);
    });
    return () => controller.abort();
  }, [open, menuRetry]);

  const reviewerGroups = groupByHarness((providers ?? []).filter((row) =>
    row.harness_id !== 'terminal' && (!row.is_proxied || row.configuration),
  ));
  const reviewerAvailable = !reviewer || reviewerGroups.some(([, rows]) => rows.some((row) =>
    (row.configuration?.id ?? row.id) === reviewer && !row.unavailable_reason && !blocksReviewCircuit(row.harness_id),
  ));

  async function start() {
    if (sending.current) return;
    sending.current = true;
    onBusyChange?.(true);
    setBusy(true);
    setError('');
    try {
      await startNodeReview(nodeId, {
        circuit_id: choice === '' ? null : Number(choice),
        max_rounds: choice === '' ? rounds : 3,
        ...(choice === '' && reviewer ? { reviewer_provider: reviewer } : {}),
      });
      if (!active.current) return;
      setOpen(false);
      setNotice('Review started. An existing live run is kept.');
    } catch (e) {
      if (!active.current) return;
      if (isAuthError(e)) onAuthFailed();
      else setError((e as Error).message);
    } finally {
      sending.current = false;
      if (active.current) { setBusy(false); onBusyChange?.(false); }
    }
  }
  return <>
    <button className="btn-ghost" onClick={() => { setNotice(''); setReviewer(''); setOpen(true); }}>Start review…</button>
    {notice && <p role="status">{notice}</p>}
    {open && <Sheet label="Start review" onClose={() => setOpen(false)} dismissible={!busy}>
      <h2>Start review</h2>
      {options === null && !error && <p role="status">Loading review choices…</p>}
      {options?.length === 0 && <p>Update the desktop to start reviews from this phone.</p>}
      {!!options?.length && <>
        <label>Review Circuit<select className="field" value={choice} disabled={busy} onChange={(e) => setChoice(e.target.value)}>
          {options.map((option) => <option key={option.circuit_id ?? 'preset'} value={option.circuit_id ?? ''}>{option.name}</option>)}
        </select></label>
        {choice === '' && <>
          <label>Reviewer launch configuration<select className="field" value={reviewer} disabled={busy || providers === null} onChange={(e) => setReviewer(e.target.value)}>
            <option value="">Default (desktop reviewer settings or this agent)</option>
            {reviewerGroups.map(([harness, rows]) => <optgroup key={harness} label={rows.find((row) => !row.configuration)?.label ?? harness}>
              {rows.map((row) => {
                const reason = row.unavailable_reason || (blocksReviewCircuit(row.harness_id) ? 'No turn-completion signal' : '');
                return <option key={row.id} value={row.configuration?.id ?? row.id} disabled={!!reason}>
                  {row.configuration?.name ?? row.label}{reason ? ` — ${reason}` : ''}
                </option>;
              })}
            </optgroup>)}
          </select></label>
          {providers === null && !providerError && <p role="status">Loading launch configurations…</p>}
          {providerError && <p role="alert" className="inline-error">{providerError} <button className="btn-ghost" disabled={busy} onClick={() => setMenuRetry((value) => value + 1)}>Retry launch configurations</button></p>}
          <label>Review rounds<input className="field" type="number" min={1} max={10} value={rounds} disabled={busy} onChange={(e) => setRounds(Number(e.target.value))} /></label>
        </>}
        <p className="field-hint">The agent must be running. Saved Circuits keep their configured rounds and reviewer. Default uses your desktop reviewer settings or this agent's provider.</p>
      </>}
      {error && <p role="alert" className="inline-error">{error}</p>}
      <button className="btn-primary" disabled={busy || !options?.some((o) => String(o.circuit_id ?? '') === choice) || (choice === '' && (!reviewerAvailable || !Number.isInteger(rounds) || rounds < 1 || rounds > 10))} onClick={() => void start()}>{busy ? 'Starting…' : 'Start review'}</button>
      <button className="btn-ghost" disabled={busy} onClick={() => setOpen(false)}>Cancel</button>
    </Sheet>}
  </>;
}
