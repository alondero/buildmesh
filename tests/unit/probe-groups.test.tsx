import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { groupedProbeTabs, rememberProbeSubview, restoredProbeSubview } from '../../src/lib/probeGroups';
import { RelatedTools } from '../../src/components/Probe/RelatedTools';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

beforeEach(() => {
  localStorage.clear();
  const mesh = { id: 1, name: 'API', path: '/api', base_ref: 'origin/trunk' };
  useMeshStore.setState({ selectedMeshId: 1, meshes: [mesh] as never, meshesById: new Map([[1, mesh]]) as never });
  seedAgentNodes([{ id: 5, mesh_id: 1, name: 'Fix API', path: '/api-work', branch: 'feature', status: 'idle', env: 'windows' } as AgentNode]);
  useAgentNodeStore.setState({ activeNodeId: 5 });
  useUIStore.setState({ probeTab: 'files', probeContextPins: {}, probeWorkingSet: { tabs: ['files'], mru: ['files'] }, viewMode: 'mesh' });
});
afterEach(cleanup);

describe('Probe group navigation', () => {
  it('persists the Files subview through the store and restores it for a new session', () => {
    useUIStore.getState().openProbeTab('review');
    expect(localStorage.getItem('buildmesh.probe-subview.files')).toBe('review');
    useUIStore.setState({ probeTab: 'usage' });
    expect(restoredProbeSubview('files')).toBe('review');
    rememberProbeSubview('files');
    expect(restoredProbeSubview('files')).toBe('files');
    localStorage.setItem('buildmesh.probe-subview.files', 'pulls');
    expect(restoredProbeSubview('files')).toBe('files');
    expect(restoredProbeSubview('usage')).toBe('usage');
  });

  it('projects one slot per group in insertion order and picks active, then most recent subviews', () => {
    expect(groupedProbeTabs(['files', 'usage', 'review', 'issues', 'pulls', 'sessions'], 'review', ['pulls', 'files', 'issues']))
      .toEqual(['review', 'usage', 'pulls', 'sessions']);
    expect(groupedProbeTabs(['files', 'usage', 'review', 'issues', 'pulls'], 'usage', ['review', 'issues', 'files', 'pulls']))
      .toEqual(['review', 'usage', 'issues']);
  });

  it('Files buttons expose selection, baselines and keyboard navigation without transferring pins', () => {
    useUIStore.setState({ probeContextPins: { review: { tab: 'review', lens: 'agent', meshId: 1, nodeId: 5 } } });
    render(<RelatedTools />);
    const group = screen.getByRole('group', { name: 'Files & changes views' });
    const explorer = within(group).getByRole('button', { name: 'Explorer' });
    const changes = within(group).getByRole('button', { name: 'Agent changes' });
    expect(explorer.getAttribute('aria-pressed')).toBe('true');
    expect(screen.getByTestId('files-baseline').textContent).toBe('Working tree vs HEAD · Repository scope');
    explorer.focus(); fireEvent.keyDown(explorer, { key: 'ArrowRight' });
    expect(document.activeElement).toBe(changes);
    expect(changes.getAttribute('aria-pressed')).toBe('true');
    expect(screen.getByTestId('files-baseline').textContent).toBe('Agent changes vs merge base (origin/trunk) · Agent scope');
    expect(useUIStore.getState().probeContextPins.files).toBeUndefined();
    fireEvent.keyDown(changes, { key: 'Home' }); expect(document.activeElement).toBe(explorer);
    fireEvent.keyDown(explorer, { key: 'End' }); expect(document.activeElement).toBe(changes);
    fireEvent.keyDown(changes, { key: 'ArrowRight' }); expect(document.activeElement).toBe(explorer);
    fireEvent.keyDown(explorer, { key: 'ArrowLeft' }); expect(document.activeElement).toBe(changes);
    act(() => useUIStore.getState().openProbeTab('files'));
    expect(explorer.getAttribute('aria-pressed')).toBe('true');
    expect(useUIStore.getState().probeContextPins.review).toEqual({ tab: 'review', lens: 'agent', meshId: 1, nodeId: 5 });
  });

  it('GitHub subviews preserve exact destinations and omit Files comparison prose', () => {
    useUIStore.getState().openProbeTab('issues');
    render(<RelatedTools />);
    const group = screen.getByRole('group', { name: 'GitHub views' });
    fireEvent.click(within(group).getByRole('button', { name: 'Pull requests' }));
    expect(useUIStore.getState().probeTab).toBe('pulls');
    expect(screen.queryByTestId('files-baseline')).toBeNull();
    act(() => useUIStore.getState().openProbeTab('issues'));
    expect(within(group).getByRole('button', { name: 'Issues' }).getAttribute('aria-pressed')).toBe('true');
  });

  it('initializes a fresh UI store with the saved Files subview', async () => {
    useUIStore.getState().openProbeTab('review');
    vi.resetModules();
    const { useUIStore: restarted } = await import('../../src/stores/uiStore');
    expect(restarted.getState().probeTab).toBe('review');
    const state = restarted.getState();
    expect(groupedProbeTabs(state.probeWorkingSet.tabs, state.probeTab, state.probeWorkingSet.mru)).toEqual(['review']);
  });
});
