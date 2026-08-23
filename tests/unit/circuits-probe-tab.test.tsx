/**
 * Tests for the Autopilot Circuits Probe tab (spec #1205 / walking
 * skeleton #1206).
 *
 * Strategy mirrors `autopilot-probe-tab.test.tsx`: mount the full
 * `ProbePanel`, click the activity-rail button, then assert on rendered
 * structure and the exact IPC contract the tab fires. Wire shapes come
 * from `src/types/generated/` so drift between the Rust structs and
 * these fixtures is caught at compile time.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { AutopilotCircuit } from '../../src/types/generated/AutopilotCircuit';
import type { CircuitRunDetail } from '../../src/types/generated/CircuitRunDetail';
// Direct wrapper access for the IPC-contract block below.
import {
  listCircuits,
  createCircuit,
  setCircuitEnabled,
  deleteCircuit,
  triggerCircuitNow,
  listCircuitRuns,
} from '../../src/lib/tauri';

const MESH: Mesh = {
  id: 42,
  name: 'demo',
  path: '/repos/demo',
  layout: 'single',
  position: 0,
  created_at: '2026-01-01',
  scratchpad: '',
  sandbox: false,
};

const CIRCUIT: AutopilotCircuit = {
  id: 7,
  mesh_id: 42,
  name: 'nightly-sweep',
  description: '',
  enabled: true,
  concurrency_limit: 1,
  graph_json: '{"version":1,"nodes":[],"edges":[]}',
  created_at: '2026-08-22 10:00:00',
  updated_at: '2026-08-22 10:00:00',
};

const RUN_DONE: CircuitRunDetail = {
  run: {
    id: 11,
    circuit_id: 7,
    mesh_id: 42,
    trigger_identity: 'manual:1724000000000',
    state: 'completed',
    context_json: '{}',
    created_at: '2026-08-22 10:05:00',
    updated_at: '2026-08-22 10:07:00',
  },
  steps: [
    {
      id: 1,
      run_id: 11,
      node_id: 'trigger',
      agent_node_id: null,
      status: 'completed',
      attempt: 1,
      outcome: 'completed',
      error_message: null,
      started_at: '2026-08-22 10:05:00',
      completed_at: '2026-08-22 10:05:00',
    },
    {
      id: 2,
      run_id: 11,
      node_id: 'spawn',
      agent_node_id: 900,
      status: 'completed',
      attempt: 1,
      outcome: 'completed',
      error_message: null,
      started_at: '2026-08-22 10:05:00',
      completed_at: '2026-08-22 10:07:00',
    },
  ],
};

const RUN_RUNNING: CircuitRunDetail = {
  run: { ...RUN_DONE.run, id: 12, state: 'running' },
  steps: [],
};

function mockBackend(overrides: {
  circuits?: AutopilotCircuit[];
  runs?: CircuitRunDetail[];
} = {}) {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    if (cmd === 'list_circuits') return Promise.resolve(overrides.circuits ?? [CIRCUIT]);
    if (cmd === 'list_circuit_runs') return Promise.resolve(overrides.runs ?? [RUN_DONE, RUN_RUNNING]);
    if (
      cmd === 'create_circuit' ||
      cmd === 'set_circuit_enabled' ||
      cmd === 'delete_circuit'
    ) {
      return Promise.resolve(args && cmd === 'create_circuit' ? CIRCUIT : undefined);
    }
    if (cmd === 'trigger_circuit_now') return Promise.resolve(13);
    // Default-fall-through so failures surface the unexpected command.
    return Promise.resolve({ cmd });
  });
}

async function openCircuitsTab() {
  const user = userEvent.setup();
  render(<ProbePanel />);
  await user.click(screen.getByRole('button', { name: 'Circuits' }));
}

beforeEach(() => {
  useMeshStore.setState({
    meshes: [MESH],
    meshesById: new Map([[MESH.id, MESH]]),
    selectedMeshId: MESH.id,
  });
  useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
  useUIStore.setState({ probeOpen: false, probeTab: 'files', activeDiffFile: null });
});

describe('CircuitsProbeTab', () => {
  it('lists circuits with their run ledger', async () => {
    mockBackend();
    await openCircuitsTab();

    expect(await screen.findByText('nightly-sweep')).toBeTruthy();
    // Both runs render with their ledger vocabulary.
    expect(await screen.findByTestId('run-state-11').then((el) => el.textContent)).toBe('completed');
    expect(screen.getByTestId('run-state-12').textContent).toBe('running');
    // Step chain renders as node:status pairs.
    expect(screen.getByText(/trigger:completed/)).toBeTruthy();
    // The load pass read both commands with camelCase args.
    expect(invoke).toHaveBeenCalledWith('list_circuits', { meshId: 42 });
    expect(invoke).toHaveBeenCalledWith('list_circuit_runs', { circuitId: 7, limit: 10 });
  });

  it('creates a circuit through the throwaway authoring form', async () => {
    mockBackend();
    const user = userEvent.setup();
    await openCircuitsTab();

    await user.type(await screen.findByTestId('circuit-name-input'), 'review-bot');
    await user.type(screen.getByTestId('circuit-prompt-input'), 'review the open PR');
    await user.click(screen.getByTestId('circuit-create-button'));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('create_circuit', {
        meshId: 42,
        name: 'review-bot',
        description: '',
        concurrencyLimit: 1,
        initialPrompt: 'review the open PR',
      });
    });
  });

  it('Trigger Now mints a manual run', async () => {
    mockBackend();
    const user = userEvent.setup();
    await openCircuitsTab();

    await user.click(await screen.findByTestId('circuit-trigger-7'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('trigger_circuit_now', { circuitId: 7 });
    });
  });

  it('toggling enable writes the flag', async () => {
    mockBackend();
    const user = userEvent.setup();
    await openCircuitsTab();

    const toggle = (await screen.findByTestId('circuit-enabled-7')) as HTMLInputElement;
    expect(toggle.checked).toBe(true);
    await user.click(toggle);
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('set_circuit_enabled', {
        circuitId: 7,
        enabled: false,
      });
    });
  });

  it('deletes a circuit', async () => {
    mockBackend();
    const user = userEvent.setup();
    await openCircuitsTab();

    await user.click(await screen.findByTestId('circuit-delete-7'));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('delete_circuit', { circuitId: 7 });
    });
  });

  it('shows the empty state when no circuits exist', async () => {
    mockBackend({ circuits: [], runs: [] });
    await openCircuitsTab();

    expect(await screen.findByText('No circuits yet')).toBeTruthy();
  });
});

describe('Autopilot Circuits IPC contract (ADR-0010 seam)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockResolvedValue([]);
  });

  // The wrappers are the ONLY sanctioned path to these commands; pin
  // each one's exact snake_case command name + camelCase arg keys so a
  // rename breaks here instead of at runtime ("command not found").
  it('every wrapper targets its registered Rust command with camelCase args', async () => {
    await listCircuits(42);
    expect(invoke).toHaveBeenLastCalledWith('list_circuits', { meshId: 42 });

    await createCircuit(42, 'n', 'd', 2, 'p');
    expect(invoke).toHaveBeenLastCalledWith('create_circuit', {
      meshId: 42,
      name: 'n',
      description: 'd',
      concurrencyLimit: 2,
      initialPrompt: 'p',
    });

    await setCircuitEnabled(7, false);
    expect(invoke).toHaveBeenLastCalledWith('set_circuit_enabled', {
      circuitId: 7,
      enabled: false,
    });

    await deleteCircuit(7);
    expect(invoke).toHaveBeenLastCalledWith('delete_circuit', { circuitId: 7 });

    await triggerCircuitNow(7);
    expect(invoke).toHaveBeenLastCalledWith('trigger_circuit_now', { circuitId: 7 });

    await listCircuitRuns(7, 5);
    expect(invoke).toHaveBeenLastCalledWith('list_circuit_runs', { circuitId: 7, limit: 5 });
  });
});
