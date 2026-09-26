import type { CircuitStepObservationCoverage } from '../../types/generated/CircuitStepObservationCoverage';
import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { circuitRunHistory, recordCircuitOutcome } from '../../lib/tauri/circuitEvidence';
import type { CircuitHistoryEntry } from '../../types/generated/CircuitHistoryEntry';
import type { CircuitCheckpoint } from '../../types/generated/CircuitCheckpoint';
import type { CheckpointAction } from '../../types/generated/CheckpointAction';
import type { RecordedObservation } from '../../types/generated/RecordedCircuitObservation';
import type { RecordedClassification } from '../../types/generated/RecordedClassification';
import type { ObservedWorkFact } from '../../types/generated/ObservedWorkFact';

const labels: Record<string, string> = {
  continuation_effect: 'Continuation prompt', step_capacity_wait: 'Step capacity wait changed', queue_wait: 'Waiting for admission', configuration_pinned: 'Pinned run configuration', evidence_window_changed: 'Evidence wait changed',
  review_continuation: 'Continued a failed review',
  run_transition: 'Run state', step_transition: 'Step state', effect_intent: 'Action intended',
  effect_possible_dispatch: 'Action may have been sent', effect_result: 'Action result',
  effect_reconciled: 'Action reconciled by read-only check', effect_target: 'Action target recorded',
  operator_attestation: 'Operator-recorded outcome (attestation)',
  observation: 'Observed evidence', evidence_recheck: 'Evidence recheck requested',
  native_hook_received: 'Native evidence received', classification: 'Report interpretation',
};

function observationDetail(detail: string): RecordedObservation | null {
  try {
    const record = JSON.parse(detail) as RecordedObservation;
    const fact = record?.observation?.fact;
    if (fact?.kind === 'ownership_snapshot' && !Array.isArray(fact.active_work)) return null;
    if (fact?.kind === 'assistant_report' && typeof fact.text !== 'string') return null;
    return record?.observation?.identity && typeof record.observation.fact?.kind === 'string'
      && Number.isFinite(new Date(record.observation.observed_at_ms).getTime())
      && typeof record.observation.source === 'string' && typeof record.disposition === 'string' ? record : null;
  } catch { return null; }
}

function factLabel(fact: ObservedWorkFact): string {
  switch (fact.kind) {
    case 'working': return 'Working';
    case 'yielded': return 'Turn yielded';
    case 'needs_input': return 'Needs input';
    case 'permission_requested': return 'Permission requested';
    case 'question_requested': return 'Question awaiting an answer';
    case 'human_wait_requested': return `Awaiting ${fact.wait_kind.replace(/_/g, ' ')}: ${fact.request_id}`;
    case 'tool_failed': return `Tool request failed or was cancelled: ${fact.request_id}`;
    case 'tool_response': return `Tool request resolved: ${fact.request_id}`;
    case 'human_response': return `Response to ${fact.wait_kind.replace(/_/g, ' ')}: ${fact.request_id}`;
    case 'foreground_terminated': return 'Foreground turn ended';
    case 'foreground_reconciled': return 'Foreground conflict resolved by validated recheck';
    case 'owned_started': return `Owned work started: ${fact.work_id}`;
    case 'owned_terminated': return `Owned work ended: ${fact.work_id}`;
    case 'ownership_covered': return 'Owned-work coverage verified';
    case 'ownership_snapshot': return `Owned work still active: ${fact.active_work.join(', ') || 'none reported'}`;
    case 'ownership_unavailable': return fact.reason;
    case 'assistant_report': return 'Complete final assistant response';
    case 'assigned_work_completed': return 'Assigned work completed';
    case 'unavailable': return 'Evidence unavailable';
  }
}

function classificationDetail(detail: string): RecordedClassification | null {
  try {
    const record = JSON.parse(detail) as RecordedClassification;
    const owner = record?.evidence_owner;
    return typeof record?.interpretation === 'string' && typeof record.lifecycle_verified === 'boolean'
      && (record.report_revision === null || typeof record.report_revision === 'string')
      && (owner === null || (owner && typeof owner.step_id === 'string' && Number.isFinite(owner.attempt))) ? record : null;
  } catch { return null; }
}

function HistoryDetail({ entry }: { entry: CircuitHistoryEntry }) {
  const classification = entry.kind === 'classification' ? classificationDetail(entry.detail) : null;
  if (classification) return <div className="space-y-1 text-text-secondary">
    <p>Interpretation: {classification.interpretation.replace(/_/g, ' ')}</p>
    <p>{classification.lifecycle_verified ? 'Bound to verified lifecycle evidence' : 'Lifecycle remains unverified; interpretation cannot advance this step'}</p>
    <p>Report revision: {classification.report_revision ?? 'unavailable'}</p>
    <p>{classification.report_completeness === 'complete' ? 'Complete final assistant response'
      : classification.report_completeness === 'partial' ? 'Partial report; completeness is unverified'
        : 'Report unavailable'}</p>
    {classification.report_text && <pre className="whitespace-pre-wrap break-words font-mono">{classification.report_text}</pre>}
    {classification.evidence_owner && <p>Evidence owner: {classification.evidence_owner.step_id} · Attempt {classification.evidence_owner.attempt}</p>}
  </div>;
  const record = entry.kind === 'observation' ? observationDetail(entry.detail) : null;
  if (!record) return <pre className="whitespace-pre-wrap break-words font-mono text-text-secondary">{entry.detail}</pre>;
  const { observation, disposition } = record;
  const { identity, fact } = observation;
  return <div className="space-y-1 text-text-secondary">
    <p>{factLabel(fact)}</p>
    <p>{disposition.replace(/_/g, ' ')} · {observation.authoritative ? 'Authoritative source' : 'Reduced confidence'}</p>
    <p>Source: {observation.source} · Observed: {observation.observed_at_ms > 0 ? new Date(observation.observed_at_ms).toISOString() : 'unavailable'}</p>
    {fact.kind === 'assistant_report' && <pre className="whitespace-pre-wrap break-words font-mono">{fact.text}</pre>}
    <details>
      <summary className="cursor-pointer">Evidence identity</summary>
      <p>Agent {identity.agent_node_id} · Session {identity.session_id ?? 'unavailable'} · Turn {identity.turn_id ?? 'unavailable'}</p>
      <p>Source event: {observation.source_id ?? 'unavailable'}</p>
      {fact.kind === 'assistant_report' && <p>Report revision: {fact.revision}</p>}
    </details>
  </div>;
}

export function CircuitEvidenceHistory({ runId, updatedAt }: {
  runId: number; updatedAt: string;
}) {
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<CircuitHistoryEntry[] | null>(null);
  const [checkpoints, setCheckpoints] = useState<CircuitCheckpoint[]>([]);
  const [coverage, setCoverage] = useState<CircuitStepObservationCoverage[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [reason, setReason] = useState('');
  const [pending, setPending] = useState(false);
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    if (!open) return;
    let current = true;
    const subscription = listen<unknown>('circuit-run-updated', ({ payload }) => {
      if (current && typeof payload === 'object' && payload !== null && 'run_id' in payload && payload.run_id === runId) {
        setRefresh((value) => value + 1);
      }
    });
    void subscription.catch((cause: unknown) => { if (current) setError(String(cause)); });
    return () => { current = false; void subscription.then((unlisten) => unlisten(), () => {}); };
  }, [open, runId]);
  useEffect(() => {
    if (!open) return;
    let current = true;
    setRows(null);
    setCheckpoints([]);
    setCoverage([]);
    setError(null);
    circuitRunHistory(runId).then(
      (next) => { if (current) { setRows(next.entries); setCheckpoints(next.checkpoints); setCoverage(next.coverage); } },
      (cause: unknown) => { if (current) setError(String(cause)); },
    );
    return () => { current = false; };
  }, [runId, updatedAt, open, refresh]);

  async function record(step: CircuitCheckpoint, action: CheckpointAction) {
    if (rows === null || pending || reason.trim() === '') return;
    setPending(true);
    setError(null);
    try {
      await recordCircuitOutcome({ run_id: runId, node_id: step.node_id, attempt: step.attempt,
        expected_revision: rows[rows.length - 1]?.id ?? 0, action, reason });
      setReason('');
      setRefresh((value) => value + 1);
    } catch (cause) { setError(String(cause)); }
    finally { setPending(false); }
  }

  return <details className="mt-2 text-2xs min-w-0" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer text-accent-cyan">Circuit Run History</summary>
    {open && <div className="mt-1 space-y-2">
      {error && <p role="alert" className="text-status-error break-words">{error}</p>}
      {rows === null && !error && <p>Loading history…</p>}
      {rows?.length === 0 && <p className="text-text-muted">No evidence history was retained for this run.</p>}
      {coverage.length > 0 && <details>
        <summary className="cursor-pointer">Current harness observation capabilities</summary>
        <ul className="space-y-2 break-words">
          {coverage.map((item) => <li key={`${item.node_id}:${item.attempt}`}>
            <p>{item.node_id} · Attempt {item.attempt} · {item.capabilities.harness}</p>
            <p>{item.platform}</p>
            <p>Foreground: {item.capabilities.foreground}</p>
            <p>Owned work: {item.capabilities.owned_work}</p>
            <p>Final report: {item.capabilities.final_report}</p>
            <p>Reconciliation: {item.capabilities.reconciliation}</p>
            {item.deadline_ms !== null && <p>Evidence deadline: {new Date(item.deadline_ms).toISOString()}</p>}
            {item.human_waits.filter((wait) => wait.resolved_at_ms === null).map((wait, index) => <div key={index}>
              <p>{item.waits_active ? 'Awaiting' : 'Retained wait:'} {wait.wait_kind.replace(/_/g, ' ')}{item.waits_active ? ' \u00b7 No automatic timeout' : ' \u00b7 Run is not waiting'}</p>
              <p>Source: {wait.source} · Observed: {new Date(wait.observed_at_ms).toISOString()}</p>
              <p>{wait.request_id ? `Request: ${wait.request_id}` : 'Request correlation unavailable; activity alone cannot resolve this wait.'}</p>
              <p>{wait.authoritative ? 'Authoritative request' : 'Reduced confidence; request is not verified'}</p>
              {item.waits_active && <p>Inspect the agent request. Matching response support depends on the harness; pause or cancel the Circuit to stop waiting.</p>}
            </div>)}
            <p>These are adapter capabilities; they do not establish live delivery for this installation.</p>
          </li>)}
        </ul>
      </details>}
      <ol className="space-y-2">
        {rows?.map((entry) => <li key={entry.id} className="break-words">
          <div className="text-text-primary">{labels[entry.kind] ?? entry.kind}</div>
          <div className="text-text-muted">{entry.observed_at}{entry.node_id && ` · ${entry.node_id}`}{entry.attempt !== null && ` · attempt ${entry.attempt}`}</div>
          <HistoryDetail entry={entry} />
        </li>)}
      </ol>
      {checkpoints.length > 0 && <div className="space-y-2">
        <p>An operator-recorded outcome is an attestation. It does not grant permission or review approval.</p>
        <label className="block">Reason and supporting evidence
          <textarea value={reason} onChange={(event) => setReason(event.target.value)} disabled={pending}
            className="block w-full min-w-0 rounded-sm border border-border-subtle bg-bg-card p-1" />
        </label>
        {checkpoints.map((step) => <fieldset key={step.node_id} disabled={pending || rows === null || reason.trim() === ''} className="flex flex-wrap gap-2">
          <legend>{step.node_id}</legend>
          {step.actions.map((action) => <button key={action} type="button" className="text-accent-cyan disabled:opacity-50" onClick={() => void record(step, action)}>
            {{ completed: 'Record completed', not_performed: 'Record not performed', retry: 'Retry as new attempt', recheck: 'Recheck evidence' }[action]}
          </button>)}
        </fieldset>)}
      </div>}
    </div>}
  </details>;
}
