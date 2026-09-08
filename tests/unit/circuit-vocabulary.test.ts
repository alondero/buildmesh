/**
 * Circuit vocabulary lock (issue #1660): UI runtime constants import the
 * generated unions and queued-step bind matches the Rust capacity policy.
 */
import { describe, expect, it } from 'vitest';
import {
  ADMITTED_RUN_STATES,
  isQueuedStepStatus,
  isTerminalRunState,
  pendingRunBind,
  queuedStepBind,
  STEP_STATUS_QUEUED,
  stepStatusLabel,
  TERMINAL_RUN_STATES,
} from '../../src/components/Circuits/circuitVocabulary';
import { parseGraph, stableGraphJson } from '../../src/components/Circuits/circuitGraphModel';
import { queuedReason } from '../../src/components/Circuits/runDiagnostics';

describe('circuit vocabulary', () => {
  it('stores Queued as pending_slot and labels it Queued', () => {
    expect(STEP_STATUS_QUEUED).toBe('pending_slot');
    expect(stepStatusLabel('pending_slot')).toBe('Queued');
    expect(stepStatusLabel('queued')).toBe('Queued');
    expect(isQueuedStepStatus('pending_slot')).toBe(true);
    expect(isQueuedStepStatus('queued')).toBe(true);
    expect(isQueuedStepStatus('running')).toBe(false);
  });

  it('treats completed/failed/cancelled as terminal run states', () => {
    expect([...TERMINAL_RUN_STATES]).toEqual(['completed', 'failed', 'cancelled']);
    expect(isTerminalRunState('completed')).toBe(true);
    expect(isTerminalRunState('paused')).toBe(false);
    expect([...ADMITTED_RUN_STATES]).toEqual(['running', 'paused']);
  });

  it('queuedStepBind prefers step slots then the agent lease', () => {
    expect(queuedStepBind(1, 1)).toBe('circuit_step_slots');
    expect(queuedStepBind(2, 1)).toBe('circuit_agent_lease');
    expect(queuedReason({
      concurrencyLimit: 1,
      runningSteps: 1,
      meshRunCapacity: 2,
      meshActiveRuns: 1,
    })).toContain('one step at a time');
    expect(queuedReason({
      concurrencyLimit: 2,
      runningSteps: 1,
      meshRunCapacity: 2,
      meshActiveRuns: 1,
    })).toContain('circuit agent slot');
  });

  it('pendingRunBind returns mesh_run_admission only when mesh is full', () => {
    expect(pendingRunBind(0, 0)).toBeNull();
    expect(pendingRunBind(2, 1)).toBeNull();
    expect(pendingRunBind(2, 2)).toBe('mesh_run_admission');
    expect(pendingRunBind(1, 5)).toBe('mesh_run_admission');
  });
});

describe('parseGraph / stableGraphJson', () => {
  it('round-trips a walking skeleton and is order-insensitive', () => {
    const json = JSON.stringify({
      version: 2,
      nodes: [
        { id: 'b', type: { type: 'notify', message: 'done' } },
        { id: 'a', type: { type: 'manual' } },
      ],
      edges: [{ from: 'a', to: 'b', condition: 'always' }],
    });
    const graph = parseGraph(json);
    expect(graph.nodes.map((n) => n.id).sort()).toEqual(['a', 'b']);
    const reordered = parseGraph(
      JSON.stringify({
        version: 2,
        nodes: [
          { id: 'a', type: { type: 'manual' } },
          { id: 'b', type: { type: 'notify', message: 'done' } },
        ],
        edges: [{ from: 'a', to: 'b', condition: 'always' }],
      })
    );
    expect(stableGraphJson(graph)).toBe(stableGraphJson(reordered));
  });
});
