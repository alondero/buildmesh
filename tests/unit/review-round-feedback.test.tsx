import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { RunProgressSummary } from '../../src/components/Circuits/RunProgressSummary';
import { reviewRoundFeedback } from '../../src/components/Circuits/runProgressModel';
import { circuitRunHistory } from '../../src/lib/tauri/circuitEvidence';
import type { CircuitRunOverview } from '../../src/types/generated/CircuitRunOverview';
import type { CircuitEvidenceView } from '../../src/types/generated/CircuitEvidenceView';

vi.mock('../../src/lib/tauri/circuitEvidence', () => ({ circuitRunHistory: vi.fn() }));

const overview = (id = 10, round = 3): CircuitRunOverview => ({
  run: { id, circuit_id: 1, mesh_id: 1, source_agent_node_id: null, trigger_identity: 'manual', state: 'completed',
    context_json: JSON.stringify({ 'node.verdict.review_verdict': 'approved', 'node.verdict.review_verdict_attempt': String(round),
      'node.verdict.evaluated_output': `Latest feedback ${id} round ${round}` }),
    created_at: '', updated_at: '2026-11-11 10:00:00', attention_dismissed_at: null },
  graph: { version: 3, nodes: [{ id: 'verdict', type: { type: 'review_verdict', target_node_id: null } }], edges: [] },
  steps: [{ id: 1, run_id: id, node_id: 'verdict', attempt: round, status: 'completed', outcome: 'completed',
    agent_node_id: null, error_message: null, started_at: null, completed_at: null }],
  circuit_name: 'Review', review_round_limit: 5, attention: { revision: 1, checkpoints: [], pushes: [] },
});

const history = (): CircuitEvidenceView => ({ entries: [
  { id: 1, node_id: 'other-gate', attempt: 1, kind: 'classification', detail: JSON.stringify({
    step_id: 'other-gate', attempt: 1, interpretation: 'completed', lifecycle_verified: true, report_text: 'Unrelated report',
  }), source: 'classifier', disposition: 'interpreted', observed_at: '' },
  { id: 2, node_id: 'verdict', attempt: 1, kind: 'classification', detail: JSON.stringify({
    step_id: 'verdict', attempt: 1, interpretation: 'working', lifecycle_verified: true, report_text: '## Round one\n\nFix the race in the terminal.',
  }), source: 'classifier', disposition: 'interpreted', observed_at: '' },
  { id: 3, node_id: 'verdict', attempt: 2, kind: 'operator_review', detail: 'Human review changes_requested: Add a regression test.',
    source: 'operator', disposition: 'changes_requested', observed_at: '' },
  { id: 0, node_id: 'verdict', attempt: 1, kind: 'step_transition', detail: 'completed',
    source: 'circuit_worker', disposition: 'applied', observed_at: '' },
], checkpoints: [], coverage: [] });

const renderRun = (value = overview()) => <RunProgressSummary overview={value} roleLabel={(id) => id} />;
const card = () => screen.getByTestId('run-latest-review');
const select = (round: number) => fireEvent.click(screen.getByRole('button', { name: new RegExp(`^Round ${round}:`) }));

beforeEach(() => { vi.mocked(circuitRunHistory).mockReset(); vi.mocked(circuitRunHistory).mockResolvedValue(history()); });

describe('review round feedback', () => {
  it('selects an earlier report and its verdict, then returns to the latest report', async () => {
    render(renderRun());
    expect(card().textContent).toContain('Latest review · Round 3');
    expect(circuitRunHistory).not.toHaveBeenCalled();
    select(1);
    await waitFor(() => expect(card().textContent).toContain('Fix the race in the terminal.'));
    expect(card().textContent).toContain('Review · Round 1');
    expect(card().textContent).toContain('Changes requested');
    expect(card().textContent).not.toContain('Latest feedback');
    expect(screen.getByRole('button', { name: /^Round 1:/ }).getAttribute('aria-pressed')).toBe('true');
    fireEvent.click(within(card()).getByRole('button', { name: 'Read the full review' }));
    expect(card().querySelector('pre')?.textContent).toContain('## Round one');
    select(3);
    expect(card().textContent).toContain('Latest feedback 10 round 3');
    expect(card().querySelector('pre')).toBeNull();
    expect(screen.getByRole('button', { name: /^Round 3:/ }).getAttribute('aria-pressed')).toBe('true');
    expect(screen.queryByRole('button', { name: /^Round 4:/ })).toBeNull();
  });

  it('shows a person’s note with attribution and supports keyboard selection', async () => {
    const user = userEvent.setup();
    render(renderRun());
    screen.getByRole('button', { name: /^Round 2:/ }).focus();
    await user.keyboard('{Enter}');
    await waitFor(() => expect(card().textContent).toContain('Add a regression test.'));
    expect(card().textContent).toContain('recorded by a person');
    expect(within(card()).getByRole('button', { name: 'Read the full note' })).toBeTruthy();
  });

  it('preserves an explicit selection through refresh, but resets when the run changes', async () => {
    const { rerender } = render(renderRun());
    select(1);
    await waitFor(() => expect(card().textContent).toContain('Fix the race'));
    rerender(renderRun(overview(10, 4)));
    await waitFor(() => expect(card().textContent).toContain('Fix the race'));
    expect(card().textContent).toContain('Review · Round 1');
    rerender(renderRun(overview(11)));
    expect(card().textContent).toContain('Latest feedback 11 round 3');
  });

  it('follows the newest report until a round is explicitly selected', () => {
    const { rerender } = render(renderRun());
    rerender(renderRun(overview(10, 4)));
    expect(card().textContent).toContain('Latest feedback 10 round 4');
  });

  it('ignores a slow response after selecting another round or run', async () => {
    let resolve!: (value: CircuitEvidenceView) => void;
    vi.mocked(circuitRunHistory).mockReturnValueOnce(new Promise((r) => { resolve = r; }));
    const { rerender } = render(renderRun());
    select(1);
    expect(card().textContent).toContain('Loading feedback');
    select(2);
    await waitFor(() => expect(card().textContent).toContain('Add a regression test.'));
    await act(async () => resolve(history()));
    expect(card().textContent).not.toContain('Fix the race');
    rerender(renderRun(overview(11)));
    expect(card().textContent).toContain('Latest feedback 11 round 3');
  });

  it('ignores a pending history response after switching runs', async () => {
    let resolve!: (value: CircuitEvidenceView) => void;
    vi.mocked(circuitRunHistory).mockReturnValueOnce(new Promise((r) => { resolve = r; }));
    const { rerender } = render(renderRun());
    select(1);
    rerender(renderRun(overview(11)));
    await act(async () => resolve(history()));
    expect(card().textContent).toContain('Latest feedback 11 round 3');
    expect(card().textContent).not.toContain('Fix the race');
  });

  it('reports a failed history read and allows retry', async () => {
    vi.mocked(circuitRunHistory).mockRejectedValueOnce(new Error('History unavailable'));
    render(renderRun());
    select(1);
    await screen.findByRole('alert');
    expect(card().textContent).not.toContain('Latest feedback');
    fireEvent.click(within(card()).getByRole('button', { name: 'Retry loading feedback' }));
    await waitFor(() => expect(card().textContent).toContain('Fix the race'));
  });

  it('does not substitute another round’s report when history is missing or malformed', async () => {
    const value = history();
    value.entries[1].detail = '{broken';
    vi.mocked(circuitRunHistory).mockResolvedValue(value);
    render(renderRun());
    select(1);
    await waitFor(() => expect(card().textContent).toContain('No feedback was retained for this round.'));
    expect(card().textContent).not.toContain('Latest feedback');
    expect(card().textContent).not.toContain('Unrelated report');
  });
});

describe('retained review decisions', () => {
  it('uses the newest accepted decision, regardless of history ordering', () => {
    const value = history();
    const entry = value.entries[1];
    value.entries.unshift({ ...entry, id: 9, detail: JSON.stringify({ step_id: 'verdict', attempt: 1,
      interpretation: 'continue', lifecycle_verified: true, report_text: 'Fix the remaining race.' }) });
    expect(reviewRoundFeedback(value.entries, 'verdict', 1)).toMatchObject({ label: 'Changes requested', report: 'Fix the remaining race.' });
  });

  it('shows accepted report handoffs without native lifecycle verification', () => {
    const value = history();
    const record = JSON.parse(value.entries[1].detail);
    value.entries[1].detail = JSON.stringify({ ...record, lifecycle_verified: false });
    expect(reviewRoundFeedback(value.entries, 'verdict', 1)?.report).toContain('Fix the race');
  });

  it.each([true, false])('excludes unverified classifications even with lifecycle_verified=%s', (verified) => {
    const value = history();
    const record = JSON.parse(value.entries[1].detail);
    value.entries[1].detail = JSON.stringify({ ...record, lifecycle_verified: verified });
    value.entries[3].detail = 'unverified';
    expect(reviewRoundFeedback(value.entries, 'verdict', 1)).toBeNull();
  });

  it.each([
    { step_id: 'other', attempt: 1, lifecycle_verified: true, interpretation: 'completed' },
    { step_id: 'verdict', attempt: 2, lifecycle_verified: true, interpretation: 'completed' },
    { step_id: 'verdict', attempt: 1, lifecycle_verified: true, interpretation: 'insufficient_evidence' },
    null,
  ])('rejects mismatched or unaccepted classifications: %j', (record) => {
    const entry = { ...history().entries[1], detail: JSON.stringify(record) };
    expect(reviewRoundFeedback([history().entries[3], entry], 'verdict', 1)).toBeNull();
  });
});
