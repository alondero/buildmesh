import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { AgentReviewButton } from '../../src/components/AgentNodeView/AgentReviewButton';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

const { trigger, list } = vi.hoisted(() => ({ trigger: vi.fn(), list: vi.fn() }));
vi.mock('../../src/lib/tauri', async importOriginal => ({
  ...await importOriginal<typeof import('../../src/lib/tauri')>(),
  triggerCircuitFromNode: trigger,
  listCircuits: list,
}));

const node = { id: 42, mesh_id: 7, name: 'Fix parser', provider: 'claude', status: 'ready' } as AgentNode;

describe('agent workflow title-bar control', () => {
  beforeEach(() => {
    trigger.mockReset().mockResolvedValue(91);
    list.mockReset().mockResolvedValue([]);
    useAgentNodeStore.setState({ circuitOwnerships: {} });
    useUIStore.setState({ probeOpen: false });
  });

  it('starts review for a finished agent and opens its Mesh Circuits', async () => {
    render(<AgentReviewButton node={node} />);
    fireEvent.click(screen.getByRole('button', { name: 'Start agent workflow' }));
    fireEvent.change(screen.getByLabelText('Maximum review rounds'), { target: { value: '5' } });
    fireEvent.click(screen.getByRole('button', { name: 'Start review' }));
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, null, 5));
    await waitFor(() => expect(useUIStore.getState().probeTab).toBe('circuits'));
    expect(useMeshStore.getState().selectedMeshId).toBe(7);
    expect(useAgentNodeStore.getState().activeNodeId).toBe(42);
  });

  it('offers saved manual Circuits and passes the selected id', async () => {
    list.mockResolvedValue([
      { id: 3, name: 'My workflow', graph_json: JSON.stringify({ nodes: [{ id: 'trigger', type: { type: 'manual' } }], edges: [] }) },
      { id: 4, name: 'Interval only', graph_json: JSON.stringify({ nodes: [{ id: 'trigger', type: { type: 'interval', interval_seconds: 60 } }], edges: [] }) },
    ]);
    render(<AgentReviewButton node={node} />);
    fireEvent.click(screen.getByRole('button', { name: 'Start agent workflow' }));
    await screen.findByRole('option', { name: 'My workflow' });
    expect(screen.queryByRole('option', { name: 'Interval only' })).toBeNull();
    fireEvent.change(screen.getByLabelText('Workflow'), { target: { value: '3' } });
    fireEvent.click(screen.getByRole('button', { name: 'Start Circuit' }));
    await waitFor(() => expect(trigger).toHaveBeenCalledWith(42, 3, 3));
  });

  it('keeps an actionable backend error in the dialog', async () => {
    trigger.mockRejectedValue('Resume the agent before starting a review.');
    render(<AgentReviewButton node={node} />);
    fireEvent.click(screen.getByRole('button', { name: 'Start agent workflow' }));
    fireEvent.click(screen.getByRole('button', { name: 'Start review' }));
    expect((await screen.findByRole('alert')).textContent).toContain('Resume the agent');
    expect(screen.getByRole('dialog')).not.toBeNull();
  });

  it('opens the active run instead of starting a duplicate', () => {
    useAgentNodeStore.setState({ circuitOwnerships: { 42: { node_id: 42, run_id: 91, circuit_id: 3, circuit_name: 'Review' } } });
    render(<AgentReviewButton node={node} />);
    fireEvent.click(screen.getByRole('button', { name: 'Start agent workflow' }));
    fireEvent.click(screen.getByRole('button', { name: 'View circuit run #91' }));
    expect(trigger).not.toHaveBeenCalled();
    expect(useUIStore.getState().probeTab).toBe('circuits');
  });

  it.each(['suspended', 'archived', 'error'])('disables starting for %s agents', status => {
    render(<AgentReviewButton node={{ ...node, status: status as AgentNode['status'] }} />);
    expect((screen.getByRole('button', { name: 'Start agent workflow' }) as HTMLButtonElement).disabled).toBe(true);
  });
});
