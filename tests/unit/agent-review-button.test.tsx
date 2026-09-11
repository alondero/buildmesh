import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { AgentReviewButton } from '../../src/components/AgentNodeView/AgentReviewButton';
import type { SpawnOption } from '../../src/lib/groups';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

const { trigger, list } = vi.hoisted(() => ({ trigger: vi.fn(), list: vi.fn() }));
vi.mock('../../src/lib/tauri', async importOriginal => ({
  ...await importOriginal<typeof import('../../src/lib/tauri')>(),
  triggerCircuitFromNode: trigger,
  listCircuits: list,
}));

function spawnOption(id: string, label: string): SpawnOption {
  return {
    id,
    label,
    icon: id,
    harness_id: id,
    provider_id: null,
    is_proxied: false,
    group_key: id,
    color: '#fff',
  };
}

/** A Proxied child of `harness` — composite id, clusters under the harness. */
function proxiedOption(id: string, label: string, harness: string): SpawnOption {
  return {
    ...spawnOption(id, label),
    harness_id: harness,
    provider_id: id.split(':')[1],
    is_proxied: true,
    group_key: harness,
  };
}

const PROVIDERS: SpawnOption[] = [
  spawnOption('claude', 'Claude Code'),
  proxiedOption('claude:minimax', 'MiniMax', 'claude'),
  spawnOption('codex', 'Codex'),
  spawnOption('terminal', 'Terminal'),
];

const node = { id: 42, mesh_id: 7, name: 'Fix parser', provider: 'claude', status: 'ready' } as AgentNode;

describe('agent workflow title-bar control', () => {
  beforeEach(() => {
    trigger.mockReset().mockResolvedValue(91);
    list.mockReset().mockResolvedValue([]);
    useAgentNodeStore.setState({ circuitOwnerships: {} });
    useUIStore.setState({ probeOpen: false, pendingCircuitRunFocus: null });
  });

  function renderButton(value: AgentNode = node) {
    return render(<AgentReviewButton node={value} providerList={PROVIDERS} />);
  }

  it('starts review for a finished agent and opens its Mesh Circuits', async () => {
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    expect(screen.getByRole('heading', { name: 'Review or circuit for Fix parser' })).toBeTruthy();
    fireEvent.change(screen.getByLabelText('Maximum review rounds'), { target: { value: '5' } });
    fireEvent.click(screen.getByRole('button', { name: 'Start review' }));
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, null, 5, null));
    await waitFor(() => expect(useUIStore.getState().probeTab).toBe('circuits'));
    expect(useMeshStore.getState().selectedMeshId).toBe(7);
    expect(useAgentNodeStore.getState().activeNodeId).toBe(42);
  });

  it('parameterises the reviewer provider for the review loop', async () => {
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    const select = screen.getByLabelText('Reviewer provider') as HTMLSelectElement;
    // Terminal is not a reviewer — it is filtered out of the picker.
    expect(screen.queryByRole('option', { name: 'Terminal' })).toBeNull();
    fireEvent.change(select, { target: { value: 'codex' } });
    fireEvent.click(screen.getByRole('button', { name: 'Start review' }));
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, null, 3, 'codex'));
  });

  it('themes its dropdowns with a defined surface token (regression: bg-surface-raised)', () => {
    // bg-surface-raised was never declared in @theme, so Tailwind v4 emitted
    // no CSS for it: the native controls fell back to the OS white field while
    // inheriting the app's light text — light-on-white in the dark theme.
    // Hold the circuit load pending so its resolved-state update can't land
    // outside act() — this test only inspects the rendered class names.
    list.mockImplementation(() => new Promise(() => {}));
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    const controls = [
      screen.getByLabelText('Workflow'),
      screen.getByLabelText('Reviewer provider'),
      screen.getByLabelText('Maximum review rounds'),
    ];
    for (const control of controls) {
      expect(control.className).not.toContain('bg-surface-raised');
      expect(control.className).toContain('bg-bg-overlay');
      expect(control.className).toContain('text-text-primary');
    }
  });

  it('groups providers by harness and nests proxied rows', async () => {
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    const select = screen.getByLabelText('Reviewer provider') as HTMLSelectElement;
    // Same bucketing as the Spawn Menu: one group per harness, Terminal gone.
    expect(Array.from(select.querySelectorAll('optgroup')).map(group => group.getAttribute('label')))
      .toEqual(['Claude Code', 'Codex']);
    const claude = select.querySelector('optgroup[label="Claude Code"]')!;
    expect(Array.from(claude.querySelectorAll('option')).map(option => option.textContent))
      .toEqual(['Claude Code', 'MiniMax']);
    // The composite id is what reaches the backend, so it must be selectable.
    fireEvent.change(select, { target: { value: 'claude:minimax' } });
    fireEvent.click(screen.getByRole('button', { name: 'Start review' }));
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, null, 3, 'claude:minimax'));
  });

  it('sends a cross-harness pick without warning about the model', async () => {
    // #1690: a cross-harness pick resolves model and effort from the picked
    // harness's own configuration, so it must not be presented as risky, and
    // the source agent's harness must not gate the choice.
    renderButton({ ...node, provider: 'claude' });
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    const select = screen.getByLabelText('Reviewer provider') as HTMLSelectElement;
    expect(Array.from(select.querySelectorAll('option')).map(option => option.textContent))
      .toContain('Codex');
    fireEvent.change(select, { target: { value: 'codex' } });
    expect(screen.queryByText(/#1690/)).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Start review' }));
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, null, 3, 'codex'));
  });

  it('resets the reviewer provider when the dialog is reopened', () => {
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    fireEvent.change(screen.getByLabelText('Reviewer provider'), { target: { value: 'codex' } });
    expect((screen.getByLabelText('Reviewer provider') as HTMLSelectElement).value).toBe('codex');
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    expect((screen.getByLabelText('Reviewer provider') as HTMLSelectElement).value).toBe('');
  });

  it('offers saved manual Circuits and passes the selected id', async () => {
    list.mockResolvedValue([
      { id: 3, name: 'My workflow', graph_json: JSON.stringify({ nodes: [{ id: 'trigger', type: { type: 'manual' } }], edges: [] }) },
      { id: 4, name: 'Interval only', graph_json: JSON.stringify({ nodes: [{ id: 'trigger', type: { type: 'interval', interval_seconds: 60 } }], edges: [] }) },
    ]);
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    await screen.findByRole('option', { name: 'My workflow' });
    expect(screen.queryByRole('option', { name: 'Interval only' })).toBeNull();
    fireEvent.change(screen.getByLabelText('Workflow'), { target: { value: '3' } });
    // An authored Circuit carries its reviewer provider in its graph, so the
    // picker is not offered — the flag below only proves the value sent is null,
    // not that the control is hidden.
    expect(screen.queryByLabelText('Reviewer provider')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Start Circuit' }));
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, 3, 3, null));
  });

  it('keeps an actionable backend error in the dialog', async () => {
    trigger.mockRejectedValue('Resume the agent before starting a review.');
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    fireEvent.click(screen.getByRole('button', { name: 'Start review' }));
    expect((await screen.findByRole('alert')).textContent).toContain('Resume the agent');
    expect(screen.getByRole('dialog')).not.toBeNull();
  });

  it('opens the active run instead of starting a duplicate', () => {
    useAgentNodeStore.setState({ circuitOwnerships: { 42: { node_id: 42, run_id: 91, circuit_id: 3, circuit_name: 'Review', state: 'running' } } });
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    fireEvent.click(screen.getByRole('button', { name: 'View circuit run #91' }));
    expect(trigger).not.toHaveBeenCalled();
    expect(useUIStore.getState().probeTab).toBe('circuits');
    // The probe opens focused on this exact run.
    expect(useUIStore.getState().pendingCircuitRunFocus).toBe(91);
  });

  it('offers cancellation instead of a competing form for an active run', async () => {
    useAgentNodeStore.setState({ circuitOwnerships: { 42: { node_id: 42, run_id: 91, circuit_id: 3, circuit_name: 'Review', state: 'paused' } } });
    const { cancelCircuitRun } = await import('../../src/lib/tauri');
    const cancel = vi.spyOn(await import('../../src/lib/tauri'), 'cancelCircuitRun').mockResolvedValue();
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    expect(screen.queryByRole('button', { name: 'Start review' })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel active workflow' }));
    await waitFor(() => expect(cancel).toHaveBeenCalledWith(91));
    cancel.mockRestore();
    void cancelCircuitRun;
  });

  it.each(['completed', 'failed', 'cancelled'] as const)('does not let terminal %s history enable an ineligible node', state => {
    useAgentNodeStore.setState({ circuitOwnerships: {
      42: { node_id: 42, run_id: 91, circuit_id: 3, circuit_name: 'Review', state },
    } });
    renderButton({ ...node, status: 'error' });
    expect((screen.getByRole('button', { name: 'Start review or circuit' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it.each(['suspended', 'archived', 'error'])('disables starting for %s agents', status => {
    renderButton({ ...node, status: status as AgentNode['status'] });
    expect((screen.getByRole('button', { name: 'Start review or circuit' }) as HTMLButtonElement).disabled).toBe(true);
  });
});
