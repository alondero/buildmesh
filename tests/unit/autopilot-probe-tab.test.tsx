import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { AutopilotProbeTab } from '../../src/components/Probe/AutopilotProbeTab';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import type { MeshRow } from '../../src/types/generated/MeshRow';
import { seedAgentNodes } from './helpers/seedAgentNodes';

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

/** Full v30 MeshRow baseline. Each test builds its own variant via
 *  `meshRow()` to flip the fields under test. Reuses the generated
 *  `MeshRow` type from `src/types/generated/MeshRow.ts` (CONTRIBUTING.md
 *  "Harness-enforced rules" + `docs/knowledge-primer.md` Anti-Patterns
 *  forbid hand-declaring a TS interface for a Rust wire type — the
 *  drift-gate on `src/types/generated/` can't see tests, so the only
 *  protection against silent drift is importing the generated type
 *  here). */
const BASE_MESH_ROW: MeshRow = {
  name: 'demo',
  build_command: null,
  run_command: null,
  model: null,
  effort: null,
  base_ref: 'origin/main',
  use_worktree: true,
  worktree_mode: null,
  default_provider: null,
  sandbox: false,
  pre_spawn_pool_size: 0,
  autopilot_enabled: false,
  autopilot_trigger_label: null,
  autopilot_concurrency_limit: 2,
  autopilot_provider: null,
  autopilot_action_on_success: null,
  root_build_command: null,
  root_run_command: null,
  autopilot_mode: 'issue_driven',
  loop_initial_prompt: null,
  loop_suffix_prompt: null,
  loop_max_iterations: null,
  loop_interval_seconds: 0,
  loop_consecutive_failures: 0,
  circuit_run_capacity: 2,
};

function meshRow(overrides: Partial<MeshRow> = {}): MeshRow {
  // Keep `autopilot_mode`'s narrow union in the resulting object so the
  // generated-type contract holds (Partial widens it to `string`).
  const merged = { ...BASE_MESH_ROW, ...overrides } as MeshRow;
  return merged;
}


beforeEach(() => {
  vi.mocked(invoke).mockReset();
  useMeshStore.setState({ meshes: [MESH], meshesById: new Map([[MESH.id,MESH]]), selectedMeshId: MESH.id });
  useUIStore.setState({ probeOpen: true, probeTab: 'autopilot', activeDiffFile: null });
  seedAgentNodes([]);
});

function backend(row: MeshRow = meshRow()) {
  vi.mocked(invoke).mockImplementation((command) => command === 'get_mesh_properties' ? Promise.resolve(row) : Promise.resolve(undefined));
}

describe('Retired legacy Autopilot', () => {
  it('retains settings for inspection and exposes no legacy dispatch or edit controls', async () => {
    backend(meshRow({ autopilot_enabled: true, autopilot_mode: 'looping', loop_initial_prompt: 'Retained initial prompt',
      loop_suffix_prompt: 'Retained suffix prompt', autopilot_trigger_label: 'legacy-ready', autopilot_concurrency_limit: 7 }));
    render(<AutopilotProbeTab />);
    await screen.findByLabelText('Max concurrent circuit runs');
    fireEvent.click(screen.getByText('Retained legacy settings (read-only)'));
    expect(screen.getByText('Retained initial prompt')).toBeTruthy();
    expect(screen.getByText('Retained suffix prompt')).toBeTruthy();
    expect(screen.getByText('legacy-ready')).toBeTruthy();
    expect(screen.getByText(/Legacy automation will not restart/)).toBeTruthy();
    expect(screen.queryByRole('button', {name: /^Start$/})).toBeNull();
    expect(screen.queryByRole('checkbox')).toBeNull();
    expect(screen.queryByRole('textbox')).toBeNull();
    expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual(['get_mesh_properties']);
  });

  it('keeps Circuit capacity separate from retained legacy concurrency', async () => {
    backend(meshRow({autopilot_concurrency_limit:7,circuit_run_capacity:3}));
    render(<AutopilotProbeTab />);
    const capacity = await screen.findByLabelText('Max concurrent circuit runs');
    expect((capacity as HTMLSelectElement).value).toBe('3');
    fireEvent.change(capacity,{target:{value:'4'}});
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('update_mesh_circuit_run_capacity',{meshId:MESH.id,capacity:4}));
    await waitFor(() => expect((capacity as HTMLSelectElement).value).toBe('4'));
    expect(screen.getByText('7', {selector:'dd'})).toBeTruthy();
    expect(vi.mocked(invoke).mock.calls.some(([command]) => command === 'update_mesh_autopilot')).toBe(false);
  });

  it('shows a failed capacity write and retains the committed value', async () => {
    vi.mocked(invoke).mockImplementation((command) => command === 'get_mesh_properties' ? Promise.resolve(meshRow({circuit_run_capacity:2})) : Promise.reject(new Error('Capacity save failed')));
    render(<AutopilotProbeTab />);
    const capacity = await screen.findByLabelText('Max concurrent circuit runs');
    fireEvent.change(capacity,{target:{value:'5'}});
    expect((await screen.findByRole('alert')).textContent).toContain('Capacity save failed');
    expect((capacity as HTMLSelectElement).value).toBe('2');
  });

  it('surfaces a read failure without offering editable legacy controls', async () => {
    vi.mocked(invoke).mockRejectedValue(new Error('Retained settings unavailable'));
    render(<AutopilotProbeTab />);
    expect((await screen.findByRole('alert')).textContent).toContain('Retained settings unavailable');
    expect(screen.queryByRole('combobox')).toBeNull();
  });

  it('navigates to Circuits without converting legacy settings or dispatching work', async () => {
    backend();
    render(<AutopilotProbeTab />);
    await screen.findByLabelText('Max concurrent circuit runs');
    fireEvent.click(screen.getByRole('button',{name:'Configure Circuits manually'}));
    expect(useUIStore.getState().probeTab).toBe('circuits');
    expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).toEqual(['get_mesh_properties']);
  });

  it('discards an old Mesh read after the selected Mesh changes', async () => {
    let resolveOld!: (row: MeshRow) => void;
    vi.mocked(invoke).mockImplementation((_command,args) => (args as {meshId:number}).meshId === MESH.id
      ? new Promise<MeshRow>((resolve) => { resolveOld = resolve; }) : Promise.resolve(meshRow({circuit_run_capacity:5})));
    render(<AutopilotProbeTab />);
    await waitFor(() => expect(invoke).toHaveBeenCalled());
    const nextMesh = {...MESH,id:MESH.id+1};
    act(() => useMeshStore.setState({ meshes:[MESH,nextMesh],meshesById:new Map([[MESH.id,MESH],[nextMesh.id,nextMesh]]),selectedMeshId:nextMesh.id }));
    const capacity = await screen.findByLabelText('Max concurrent circuit runs');
    expect((capacity as HTMLSelectElement).value).toBe('5');
    await act(async () => resolveOld(meshRow({circuit_run_capacity:1})));
    expect((screen.getByLabelText('Max concurrent circuit runs') as HTMLSelectElement).value).toBe('5');
  });
});
