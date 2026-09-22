import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { AgentReviewButton } from '../../src/components/AgentNodeView/AgentReviewButton';
import type { SpawnOption } from '../../src/lib/groups';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

const { trigger, list, isRunning } = vi.hoisted(() => ({ trigger: vi.fn(), list: vi.fn(), isRunning: vi.fn() }));
vi.mock('../../src/lib/tauri', async importOriginal => ({
  ...await importOriginal<typeof import('../../src/lib/tauri')>(),
  triggerCircuitFromNode: trigger,
  listCircuits: list,
  isAgentRunning: isRunning,
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
    // Default: no live agent process. Existing tests use status: 'ready' so
    // the DB status alone makes the button eligible; the liveness check is
    // only consulted for transient statuses (pending/spawning).
    isRunning.mockReset().mockResolvedValue(false);
    useAgentNodeStore.setState({ circuitOwnerships: {} });
    useUIStore.setState({ probeOpen: false, pendingCircuitRunFocus: null });
  });

  function renderButton(value: AgentNode = node, providers: SpawnOption[] = PROVIDERS) {
    return render(<AgentReviewButton node={value} providerList={providers} />);
  }

  // A harness with neither a native attention hook nor a passive turn watcher
  // can never reach a yielded status, so `await_source` and `verdict` would
  // park forever. The button must not mint that run.
  it('disables the control and explains why when the node harness cannot yield a turn', () => {
    renderButton({ ...node, provider: 'freebuff' });
    const button = screen.getByRole('button', { name: 'Start review or circuit' }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(button.title).toContain('cannot run a review circuit');
  });

  it('still enables an attention-capable harness', () => {
    renderButton({ ...node, provider: 'codex' });
    const button = screen.getByRole('button', { name: 'Start review or circuit' }) as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    expect(button.title).toBe('Start review or circuit');
  });

  // Issue #1775: Cline provisions a native attention hook, so it is no longer
  // outside the review contract.
  it('enables the control for Cline after its attention hook landed', () => {
    renderButton({ ...node, provider: 'cline' });
    const button = screen.getByRole('button', { name: 'Start review or circuit' }) as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    expect(button.title).toBe('Start review or circuit');
  });

  // `AgentNode.provider` is an opaque string: it carries legacy ids and
  // user-defined harness profile ids, both of which resolve to a real executor
  // at the spawn seam. Blocking those would remove a working review from the
  // user, so the gate only fires on harnesses it can positively judge.
  it.each(['command-code', 'claude_code', '', 'my-custom-profile'])(
    'does not disable the control for the non-canonical provider id %p',
    provider => {
      renderButton({ ...node, provider });
      const button = screen.getByRole('button', { name: 'Start review or circuit' }) as HTMLButtonElement;
      expect(button.disabled).toBe(false);
      expect(button.title).toBe('Start review or circuit');
    },
  );

  it('disables the control for a legacy id that resolves to a blocked harness', () => {
    renderButton({ ...node, provider: 'deepseek' });
    expect((screen.getByRole('button', { name: 'Start review or circuit' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('greys out a reviewer harness that cannot yield a turn', () => {
    renderButton(node, [...PROVIDERS, spawnOption('cline', 'Cline'), spawnOption('freebuff', 'Freebuff')]);
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    const select = screen.getByLabelText('Reviewer provider') as HTMLSelectElement;
    const options = Array.from(select.querySelectorAll('option'));
    expect(options.find(o => o.value === 'codex')?.disabled).toBe(false);
    // Issue #1775: Cline carries a hook now, so its row is pickable.
    expect(options.find(o => o.value === 'cline')?.disabled).toBe(false);
    expect(options.find(o => o.value === 'freebuff')?.disabled).toBe(true);
    expect(options.find(o => o.value === 'freebuff')?.textContent).toBe('Freebuff (no review support)');
    // Terminal stays filtered out entirely — it is not an agent.
    expect(options.find(o => o.value === 'terminal')).toBeUndefined();
  });

  it('starts review for a finished agent and opens its Mesh Circuits', async () => {
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    expect(screen.getByRole('heading', { name: 'Review or circuit for Fix parser' })).toBeTruthy();
    fireEvent.change(screen.getByLabelText('Maximum review rounds'), { target: { value: '5' } });
    fireEvent.click(screen.getByRole('button', { name: 'Start review' }));
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, null, 5, null, false));
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
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, null, 3, 'codex', false));
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
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, null, 3, 'claude:minimax', false));
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
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, null, 3, 'codex', false));
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
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, 3, 3, null, false));
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

  // Liveness-based eligibility: the backend's `trigger_circuit_from_node`
  // gate trusts `PROCESS_REGISTRY.is_alive()` over the DB status. A node whose
  // row is still `pending`/`spawning` (in-flight, or stranded by an upstream
  // fire-and-forget swallow) is still eligible if its PTY reader is actually
  // registered — see commands::circuit::trigger_circuit_from_node.
  it.each(['pending', 'spawning'])('enables a %s node whose process is alive', async status => {
    isRunning.mockResolvedValue(true);
    renderButton({ ...node, status: status as AgentNode['status'] });
    // The hook polls the backend; wait for the resolution to land.
    await waitFor(() => {
      expect((screen.getByRole('button', { name: 'Start review or circuit' }) as HTMLButtonElement).disabled).toBe(false);
    });
  });

  it.each(['pending', 'spawning'])('keeps a %s node disabled while its process is not yet alive', status => {
    isRunning.mockResolvedValue(false);
    renderButton({ ...node, status: status as AgentNode['status'] });
    // Default mock returns false synchronously, so the initial state shows disabled.
    expect((screen.getByRole('button', { name: 'Start review or circuit' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('does not poll liveness once the node reaches a terminal status', async () => {
    // A `running` row is eligible purely by status; the liveness check is
    // only consulted while the status is transient. Pin this so a future
    // refactor doesn't waste IPC on every button render.
    const callCount = { n: 0 };
    isRunning.mockImplementation(() => {
      callCount.n += 1;
      return Promise.resolve(true);
    });
    renderButton({ ...node, status: 'running' });
    // Let any pending microtask resolve so we can compare counts after.
    await Promise.resolve();
    expect(callCount.n).toBe(0);
  });

  it('forwards the readiness-gate override (#1792) when the user opts in', async () => {
    // The override lets the user mint a run on a never-observed source
    // (no `cli_session_id`, no readable `assistant_report`); the backend
    // records it on the run's `context_json` for audit.
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    fireEvent.click(screen.getByLabelText(/Review an agent that hasn.t started yet/));
    fireEvent.click(screen.getByRole('button', { name: 'Start review' }));
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, null, 3, null, true));
  });

  it('resets the readiness-gate override when the dialog is reopened', async () => {
    // The override is intentionally off by default so power users opt in
    // by choice on every review; it does not leak across modal opens.
    // `async` + `findByLabelText` so the async `listCircuits` state update
    // (triggered by every modal open) settles inside `act(...)` and the
    // test runs without the "An update to AgentReviewButton inside a
    // test was not wrapped in act(...)" warning. The same pattern as
    // `offers saved manual Circuits and passes the selected id`.
    renderButton();
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    const override = await screen.findByLabelText(/Review an agent that hasn.t started yet/);
    fireEvent.click(override);
    expect((override as HTMLInputElement).checked).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    fireEvent.click(screen.getByRole('button', { name: 'Start review or circuit' }));
    const reopened = await screen.findByLabelText(/Review an agent that hasn.t started yet/);
    expect((reopened as HTMLInputElement).checked).toBe(false);
  });
});
