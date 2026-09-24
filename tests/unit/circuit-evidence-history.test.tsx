import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, expect, it, vi } from 'vitest';
import { emit } from '@tauri-apps/api/event';
import { CircuitEvidenceHistory } from '../../src/components/Circuits/CircuitEvidenceHistory';
import { circuitRunHistory, recordCircuitOutcome } from '../../src/lib/tauri/circuitEvidence';
import type { CircuitHistoryEntry } from '../../src/types/generated/CircuitHistoryEntry';
import type { CircuitEvidenceView } from '../../src/types/generated/CircuitEvidenceView';

vi.mock('../../src/lib/tauri/circuitEvidence', () => ({
  circuitRunHistory: vi.fn(), recordCircuitOutcome: vi.fn(),
}));

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(circuitRunHistory).mockReset();
  vi.mocked(recordCircuitOutcome).mockReset();
});

const entry: CircuitHistoryEntry = { id: 7, node_id: 'comment', attempt: 1,
  kind: 'effect_possible_dispatch', detail: 'github', observed_at: '2026-09-24T12:00:00Z' };

it('renders provenance and the entire final response without a JSON wrapper or clipping', async () => {
  const report = `${'A complete review finding.\n'.repeat(1500)}FINAL FINDING PRESERVED`;
  vi.mocked(circuitRunHistory).mockResolvedValue({ entries: [{ ...entry,kind:'observation',detail:JSON.stringify({
    disposition:'accepted',observation:{identity:{run_id:3,step_id:'comment',attempt:1,agent_node_id:9,
      session_incarnation:'1000',session_id:'session',turn_id:'turn',report_revision:null},
    source:'codex_rollout_task_complete',source_id:'event-1',observed_at_ms:1790251200000,authoritative:true,
    fact:{kind:'assistant_report',text:report,revision:'report-1'}},
  }) }],coverage: [], checkpoints:[] });
  const {container}=render(<CircuitEvidenceHistory runId={3} updatedAt="one"/>);
  fireEvent.click(container.querySelector('summary')!);
  expect(await screen.findByText('Complete final assistant response')).toBeTruthy();
  expect(screen.getByText(/Authoritative source/)).toBeTruthy();
  expect(screen.getByText(/Source: codex_rollout_task_complete/)).toBeTruthy();
  expect(container.querySelector('pre')?.textContent).toBe(report);
  expect(container.querySelector('pre')?.className).not.toMatch(/line-clamp|truncate|max-h/);
  fireEvent.click(screen.getByText('Evidence identity'));
  expect(screen.getByText('Report revision: report-1')).toBeTruthy();
});

it('refreshes external operator updates even when the run timestamp is unchanged', async () => {
  vi.mocked(circuitRunHistory).mockResolvedValue({entries:[entry], coverage: [], checkpoints:[]});
  const {container} = render(<CircuitEvidenceHistory runId={3} updatedAt="unchanged" />);
  fireEvent.click(container.querySelector('summary')!);
  await screen.findByText('Action may have been sent');
  vi.mocked(circuitRunHistory).mockResolvedValue({entries:[entry, {...entry,id:8,kind:'operator_attestation',detail:'Recorded in another window'}],coverage: [], checkpoints:[]});
  await act(async () => { await emit('circuit-run-updated',{run_id:4,state:'running'}); });
  expect(circuitRunHistory).toHaveBeenCalledTimes(1);
  await act(async () => { await emit('circuit-run-updated',{run_id:3,state:'running'}); });
  expect(await screen.findByText('Recorded in another window')).toBeTruthy();
});

it('shows ordered evidence and sends a reasoned outcome against the displayed revision', async () => {
  vi.mocked(circuitRunHistory).mockResolvedValue({ entries: [entry], coverage: [], checkpoints: [{ node_id: 'comment', attempt: 1, actions: ['completed', 'not_performed'] }] });
  vi.mocked(recordCircuitOutcome).mockResolvedValue(undefined);
  const { container } = render(<CircuitEvidenceHistory runId={3} updatedAt="one" />);
  fireEvent.click(container.querySelector('summary')!);
  expect(await screen.findByText('Action may have been sent')).toBeTruthy();
  expect(screen.getByText(/2026-09-24T12:00:00Z/)).toBeTruthy();
  fireEvent.change(screen.getByLabelText('Reason and supporting evidence'), { target: { value: 'The remote comment exists.' } });
  fireEvent.click(screen.getByRole('button', { name: 'Record completed' }));
  await waitFor(() => expect(recordCircuitOutcome).toHaveBeenCalledWith({
    run_id: 3, node_id: 'comment', attempt: 1, expected_revision: 7, action: 'completed', reason: 'The remote comment exists.',
  }));
  expect(screen.getByText(/does not grant permission or review approval/)).toBeTruthy();
});

it('ignores an older run history arriving after the selected run changes', async () => {
  let resolveOld!: (value: CircuitEvidenceView) => void;
  vi.mocked(circuitRunHistory).mockReturnValueOnce(new Promise((resolve) => { resolveOld = resolve; }))
    .mockResolvedValueOnce({ entries: [{ ...entry, id: 8, detail: 'Current evidence' }], coverage: [], checkpoints: [] });
  const { container, rerender } = render(<CircuitEvidenceHistory runId={3} updatedAt="one" />);
  fireEvent.click(container.querySelector('summary')!);
  await waitFor(() => expect(circuitRunHistory).toHaveBeenCalledWith(3));
  rerender(<CircuitEvidenceHistory runId={4} updatedAt="two" />);
  expect(await screen.findByText('Current evidence')).toBeTruthy();
  await act(async () => resolveOld({ entries: [{ ...entry, detail: 'Obsolete evidence' }], coverage: [], checkpoints: [] }));
  expect(screen.queryByText('Obsolete evidence')).toBeNull();
});

it('keeps an interpretation separate from lifecycle proof and shows its exact report revision', async () => {
  vi.mocked(circuitRunHistory).mockResolvedValue({ entries: [{ ...entry, kind: 'classification', detail: JSON.stringify({
    step_id: 'verdict', attempt: 1, interpretation: 'completed', lifecycle_verified: false,
    report_text: 'Bounded terminal tail', report_completeness: 'partial',
    report_revision: 'report-exact', evidence_owner: {run_id:3,step_id:'reviewer',attempt:1,agent_node_id:9,session_id:'session',session_incarnation:'1000',turn_id:'turn',report_revision:'report-exact'},
  }) }], coverage: [], checkpoints: [] });
  const {container}=render(<CircuitEvidenceHistory runId={3} updatedAt="one"/>);
  fireEvent.click(container.querySelector('summary')!);
  expect(await screen.findByText('Report interpretation')).toBeTruthy();
  expect(screen.getByText('Interpretation: completed')).toBeTruthy();
  expect(screen.getByText('Lifecycle remains unverified; interpretation cannot advance this step')).toBeTruthy();
  expect(screen.getByText('Report revision: report-exact')).toBeTruthy();
  expect(screen.getByText('Partial report; completeness is unverified')).toBeTruthy();
  expect(screen.getByText('Bounded terminal tail')).toBeTruthy();
  expect(screen.getByText('Evidence owner: reviewer \u00b7 Attempt 1')).toBeTruthy();
});


it('shows unsupported ownership and the evidence deadline without promising live delivery', async () => {
  vi.mocked(circuitRunHistory).mockResolvedValue({ entries: [], checkpoints: [], coverage: [{
    node_id: 'reviewer', attempt: 2, platform: 'windows host / windows launch', deadline_ms: 1790251200000, waits_active: true, human_waits: [],
    capabilities: { harness: 'codex', foreground: 'Identity-bound rollout task_complete pull',
      owned_work: 'Unavailable: rollouts do not expose a complete child/background registry',
      final_report: 'Scrubbed task_complete response', reconciliation: 'Bounded native rollout pull',
      yielded_budget_ms: 60000, active_budget_ms: 7200000 },
  }] });
  const {container} = render(<CircuitEvidenceHistory runId={3} updatedAt="one" />);
  fireEvent.click(container.querySelector('summary')!);
  fireEvent.click(await screen.findByText('Current harness observation capabilities'));
  expect(screen.getByText('Owned work: Unavailable: rollouts do not expose a complete child/background registry')).toBeTruthy();
  expect(screen.getByText('windows host / windows launch')).toBeTruthy();
  expect(screen.getByText('Evidence deadline: 2026-09-24T12:00:00.000Z')).toBeTruthy();
  expect(screen.getByText(/do not establish live delivery/)).toBeTruthy();
});


it.each([true, false])('keeps wait provenance distinct when active=%s', async (active) => {
  vi.mocked(circuitRunHistory).mockResolvedValue({ entries: [], checkpoints: [], coverage: [{
    node_id: 'reviewer', attempt: 2, platform: 'windows host / windows launch', deadline_ms: null, waits_active: active,
    capabilities: { harness: 'codex', foreground: 'Native pull', owned_work: 'Unavailable',
      final_report: 'Native report', reconciliation: 'Bounded pull', yielded_budget_ms: 60000, active_budget_ms: 7200000 },
    human_waits: ['permission', 'question'].map((kind) => ({ wait_kind: kind as 'permission' | 'question',
      request_id: kind === 'permission' ? 'tool-1' : null, source: 'native', source_id: 'source-1',
      observed_at_ms: 1790251200000, authoritative: false, resolved_at_ms: null,
      identity: { run_id: 3, step_id: 'reviewer', attempt: 2, agent_node_id: 7,
        session_incarnation: 'incarnation', session_id: 'session', turn_id: 'turn', report_revision: null },
    })),
  }] });
  const {container} = render(<CircuitEvidenceHistory runId={3} updatedAt="one" />);
  fireEvent.click(container.querySelector('summary')!);
  fireEvent.click(await screen.findByText('Current harness observation capabilities'));
  if (active) {
    expect(screen.getByText(/Awaiting permission.*No automatic timeout/)).toBeTruthy();
    expect(screen.getByText(/Awaiting question.*No automatic timeout/)).toBeTruthy();
  } else {
    expect(screen.getByText(/Retained wait: permission.*Run is not waiting/)).toBeTruthy();
    expect(screen.getByText(/Retained wait: question.*Run is not waiting/)).toBeTruthy();
    expect(screen.queryByText(/pause or cancel the Circuit/)).toBeNull();
  }
  expect(screen.getAllByText('Reduced confidence; request is not verified')).toHaveLength(2);
  expect(screen.getByText('Request: tool-1')).toBeTruthy();
  expect(screen.getByText(/Request correlation unavailable/)).toBeTruthy();
  expect(screen.queryByText(/Evidence deadline:/)).toBeNull();
});
