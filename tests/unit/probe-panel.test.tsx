import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'single',
  position: 0,
  created_at: new Date(0).toISOString(),
};

const NODE: AgentNode = {
  id: 7,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo/worktrees/agent-1',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'running',
  use_worktree: true,
  position: 0,
  created_at: new Date(0).toISOString(),
};

/** The six tabs in the order the activity bar presents them (issue #374). */
const TAB_LABELS = [
  'Project Files',
  'Agent Changes',
  'Worktree Manager',
  'Mesh Properties',
  'Git Issues',
  'Session History',
];

function tabButton(label: string): HTMLElement {
  return screen.getByRole('button', { name: label });
}

describe('ProbePanel', () => {
  beforeEach(() => {
    // Default to a context with both a selected mesh and a focused node so the
    // body renders content rather than an empty state. Individual tests narrow
    // this to exercise the empty-state branches.
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useUIStore.setState({ probeOpen: false, probeTab: 'files', activeDiffFile: null });
  });

  it('renders an activity-bar button for all six tabs even when collapsed', () => {
    render(<ProbePanel />);
    for (const label of TAB_LABELS) {
      expect(tabButton(label)).toBeTruthy();
    }
  });

  it('keeps the body collapsed until a tab is clicked', () => {
    render(<ProbePanel />);
    expect(screen.queryByRole('region', { name: 'Probe panel' })).toBeNull();

    fireEvent.click(tabButton('Project Files'));
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().probeTab).toBe('files');
    expect(screen.getByRole('region', { name: 'Probe panel' })).toBeTruthy();
  });

  it('collapses when the active tab icon is clicked a second time', () => {
    useUIStore.setState({ probeOpen: true, probeTab: 'files' });
    render(<ProbePanel />);

    fireEvent.click(tabButton('Project Files'));
    expect(useUIStore.getState().probeOpen).toBe(false);
  });

  it('switches tab (and stays open) when a different icon is clicked', () => {
    useUIStore.setState({ probeOpen: true, probeTab: 'files' });
    render(<ProbePanel />);

    fireEvent.click(tabButton('Git Issues'));
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().probeTab).toBe('issues');
  });

  it('collapses via the header close button', () => {
    useUIStore.setState({ probeOpen: true, probeTab: 'properties' });
    render(<ProbePanel />);

    fireEvent.click(screen.getByRole('button', { name: 'Close panel' }));
    expect(useUIStore.getState().probeOpen).toBe(false);
  });

  it('shows the active tab label in the header', () => {
    useUIStore.setState({ probeOpen: true, probeTab: 'sessions' });
    render(<ProbePanel />);

    const header = screen.getByRole('region', { name: 'Probe panel' });
    expect(header.textContent).toContain('Session History');
  });

  it('renders a friendly empty state when no project is active', () => {
    useMeshStore.setState({ meshesById: new Map(), selectedMeshId: null });
    useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
    useUIStore.setState({ probeOpen: true, probeTab: 'files' });
    render(<ProbePanel />);

    expect(screen.getByText('No project selected')).toBeTruthy();
  });

  it('renders a node-specific empty state on the review tab when no node is focused', () => {
    // A mesh is selected but no agent node is focused: files/properties can
    // still anchor on the mesh root, but "Agent Changes" has nothing to review.
    useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
    useUIStore.setState({ probeOpen: true, probeTab: 'review' });
    render(<ProbePanel />);

    expect(screen.getByText('No active agent node')).toBeTruthy();
  });
});
