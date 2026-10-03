import { act, cleanup, fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { GridControls } from '../../src/components/TitleBar/GridControls';
import { AttentionList } from '../../src/components/Sidebar/AttentionList';
import { AgentHistoryTab } from '../../src/components/Probe/AgentHistoryTab';
import { ReadinessSteps } from '../../src/components/AgentNodeView/ReadinessSteps';
import { CanvasEmptyStateContainer } from '../../src/components/AgentNodeView/CanvasEmptyStateContainer';
import { useSettingsResources } from '../../src/components/AppSettings/useSettingsResources';
import { FileTree } from '../../src/components/FileTree/FileTree';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useNodeActivityStore } from '../../src/stores/nodeActivityStore';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';
import { __resetSharedProviderListForTests } from '../../src/hooks/useProviderList';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const meshA = { id: 1, name: 'API repository', path: '/api', base_ref: 'main' };
const meshB = { id: 2, name: 'Web repository', path: '/web', base_ref: 'main' };
function node(id: number, name: string, status: AgentNode['status'], meshId = 1): AgentNode {
  return { id, mesh_id: meshId, name, status, path: '/api', branch: 'main', env: 'windows', provider: 'claude', use_worktree: false, created_at: '2026-10-02', cli_session_id: 'saved-session' } as AgentNode;
}
const route = { id: 'claude', label: 'Claude Code', harness_id: 'claude', group_key: 'claude', provider_id: null, is_proxied: false, color: '#fff', icon: '', resumable: true, capabilities: { harness_id: 'anthropic', is_plain_terminal: false, available_on: ['windows'], effort_control: { kind: 'none' }, supports_model_override: false, supports_effort_override: false } } as ProviderInfo;
function mockSettings(providers: Promise<ProviderInfo[]>, prefs = Promise.resolve({ default_provider: 'claude', reviewer_provider: null, naming_provider: null, harness_defaults: {}, provider_pairings: [] })) {
  vi.mocked(invoke).mockImplementation(cmd => {
    if (cmd === 'get_app_preferences') return prefs;
    if (cmd === 'list_routing_options') return Promise.resolve([route]);
    if (cmd === 'list_providers') return providers;
    if (['get_provider_accounts', 'get_keyed_first_class_catalog', 'get_provider_pairings', 'get_pairing_verifications', 'list_device_sessions', 'list_spawn_configurations'].includes(cmd)) return Promise.resolve([]);
    return Promise.resolve({});
  });
}
beforeEach(() => {
  localStorage.clear();
  __resetProviderCachesForTests(); __resetSharedProviderListForTests();
  useUIStore.getState().resetGridControls();
  useMeshStore.setState({ meshes: [meshA, meshB] as never, meshesById: new Map([[1, meshA], [2, meshB]]) as never });
  seedAgentNodes([]);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe('October desktop audit follow-up', () => {
  it('Settings has one tabbable tab, wraps vertical keys and wires panels', async () => {
    mockSettings(Promise.resolve([route]));
    render(<AppSettingsModal onClose={vi.fn()} />);
    const general = screen.getByRole('tab', { name: 'General' });
    general.focus(); fireEvent.keyDown(general, { key: 'ArrowUp' });
    const remote = screen.getByRole('tab', { name: 'Remote Access' });
    expect(document.activeElement).toBe(remote);
    expect(remote.getAttribute('aria-selected')).toBe('true');
    expect(screen.getAllByRole('tab').filter(t => t.tabIndex === 0)).toEqual([remote]);
    const panel = document.getElementById(remote.getAttribute('aria-controls')!);
    expect(panel?.getAttribute('aria-labelledby')).toBe(remote.id);
    expect(panel?.hidden).toBe(false);
    fireEvent.keyDown(remote, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(general);
    fireEvent.keyDown(general, { key: 'End' }); expect(document.activeElement).toBe(remote);
    fireEvent.keyDown(remote, { key: 'Home' }); expect(document.activeElement).toBe(general);
  });

  it('routing choices work before slow probes; a late failure keeps defaults usable and exposes Retry', async () => {
    const probes = deferred<ProviderInfo[]>(); mockSettings(probes.promise);
    render(<AppSettingsModal onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole('tab', { name: 'Providers' }));
    const picker = screen.getByLabelText('Default provider') as HTMLButtonElement;
    await waitFor(() => expect(picker.disabled).toBe(false));
    expect(picker.textContent).toContain('Claude Code');
    expect(screen.getByText(/Saved routing choices are ready/)).toBeTruthy();
    await act(async () => probes.reject(new Error('WSL probe unavailable')));
    expect(picker.disabled).toBe(false);
    expect(screen.getAllByTestId('resource-load-providers')[0].textContent).toContain('WSL probe unavailable');
    expect(screen.getByRole('button', { name: /Retry loading providers/i })).toBeTruthy();
    expect(vi.mocked(invoke).mock.calls.some(([cmd]) => cmd === 'set_app_default_provider')).toBe(false);
  });

  it('failed preferences keep routing pickers disabled even when both choice sources succeed', async () => {
    const prefs = deferred<{ default_provider: string; reviewer_provider: null; naming_provider: null; harness_defaults: object; provider_pairings: never[] }>();
    mockSettings(Promise.resolve([route]), prefs.promise);
    render(<AppSettingsModal onClose={vi.fn()} />);
    await act(async () => prefs.reject(new Error('Preferences unreadable')));
    fireEvent.click(screen.getByRole('tab', { name: 'Providers' }));
    for (const label of ['Default provider', 'Reviewer provider', 'Auto-naming']) expect((screen.getByLabelText(label) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getAllByTestId('resource-load-preferences')[0].textContent).toContain('Preferences unreadable');
    expect(vi.mocked(invoke).mock.calls.some(([cmd]) => cmd.startsWith('set_'))).toBe(false);
  });

  it.each(['success', 'failure'] as const)('a stale routing %s cannot replace a newer retry, including after unmount', async stale => {
    const first = deferred<ProviderInfo[]>(); const second = deferred<ProviderInfo[]>();
    vi.mocked(invoke).mockImplementationOnce(() => first.promise).mockImplementationOnce(() => second.promise);
    const loaded = vi.fn();
    const { result, unmount } = renderHook(() => useSettingsResources({ onRoutingLoaded: loaded }));
    let oldRequest!: Promise<unknown>; let newRequest!: Promise<unknown>;
    act(() => { oldRequest = result.current.loadRouting(); newRequest = result.current.loadRouting(); });
    await act(async () => { second.resolve([route]); await newRequest; });
    await act(async () => { if (stale === 'success') first.resolve([]); else first.reject(new Error('Old probe')); await oldRequest; });
    expect(result.current.resources.routing.status).toBe('loaded');
    expect(loaded).toHaveBeenCalledExactlyOnceWith([route]);
    const departed = deferred<ProviderInfo[]>(); vi.mocked(invoke).mockReturnValueOnce(departed.promise);
    act(() => { oldRequest = result.current.loadRouting(); }); unmount();
    await act(async () => { departed.resolve([]); await oldRequest; });
    expect(loaded).toHaveBeenCalledTimes(1);
  });

  it('Terminal creation rejects once, blocks repeated clicks while pending and permits retry', async () => {
    vi.mocked(invoke).mockResolvedValue([]);
    const creating = deferred<AgentNode>();
    const create = vi.spyOn(useAgentNodeStore.getState(), 'selectProviderForMesh').mockReturnValue(creating.promise);
    render(<CanvasEmptyStateContainer viewMode="all" lastNonSingleMode="all" selectedMeshId={1} activeNodeId={null} agentNodes={[]} visibleNodesLength={0} />);
    const start = screen.getByRole('button', { name: 'Start Terminal' });
    fireEvent.click(start); fireEvent.click(start);
    expect(create).toHaveBeenCalledTimes(1);
    expect((start as HTMLButtonElement).disabled).toBe(true);
    await act(async () => creating.reject(new Error('Repository unavailable')));
    await waitFor(() => expect((start as HTMLButtonElement).disabled).toBe(false));
    const { useToastStore } = await import('../../src/stores/toastStore');
    expect(useToastStore.getState().toasts.some(t => t.message === 'Repository unavailable')).toBe(true);
    create.mockResolvedValue(node(8, 'Terminal', 'idle'));
    fireEvent.click(start);
    await waitFor(() => expect(create).toHaveBeenCalledTimes(2));
  });

  it('FileTree retry replaces a long read failure with real file rows', async () => {
    let attempts = 0;
    vi.mocked(invoke).mockImplementation(cmd => {
      if (cmd === 'list_directory') return ++attempts === 1 ? Promise.reject(new Error('unavailable/'.repeat(50)))
        : Promise.resolve({ path: '/api', name: 'api', is_dir: true, children: [{ path: '/api/README.md', name: 'README.md', is_dir: false, children: [] }] });
      return Promise.resolve([]);
    });
    render(<FileTree rootPath="/api" showGitStatus={false} selectedFile={null} onFileSelect={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Retry files' }));
    expect((await screen.findByRole('treeitem')).textContent).toContain('README.md');
    expect(screen.queryByRole('alert')).toBeNull(); expect(attempts).toBe(2);
  });

  it('combines provider and status filters, exposes zero results and resets the persisted controls', () => {
    seedAgentNodes([node(1, 'API fix', 'error'), { ...node(2, 'Web fix', 'running', 2), provider: 'codex' }]);
    render(<GridControls />);
    fireEvent.click(screen.getByRole('button', { name: /Filters and sort:/ }));
    fireEvent.change(screen.getByLabelText('Filter provider'), { target: { value: 'codex' } });
    fireEvent.change(screen.getByLabelText('Filter status'), { target: { value: 'error' } });
    fireEvent.change(screen.getByLabelText('Sort nodes'), { target: { value: 'name' } });
    fireEvent.change(screen.getByLabelText('Sort direction'), { target: { value: 'desc' } });
    expect(screen.getByRole('status').textContent).toBe('0 of 2 nodes');
    expect(screen.getByRole('button', { name: 'Provider: codex ×' })).toBeTruthy();
    expect(JSON.parse(localStorage.getItem('buildmesh.grid-controls')!).sortDirection).toBe('desc');
    fireEvent.click(screen.getByRole('button', { name: 'Clear all filters and sorting' }));
    expect(screen.getByRole('status').textContent).toBe('2 of 2 nodes');
    expect(JSON.parse(localStorage.getItem('buildmesh.grid-controls')!)).toEqual({ searchQuery: '', provider: null, status: null, sortBy: 'custom', sortDirection: 'asc' });
  });

  it('triage finds failed and waiting agents across repositories and keeps recovery failures visible', async () => {
    seedAgentNodes([node(1, 'Needs-answer', 'awaiting_input'), node(2, 'Failed-build', 'error', 2), node(3, 'Healthy', 'running'), node(4, 'Lost-report', 'lost', 2)]);
    const recovery = vi.spyOn(useAgentNodeStore.getState(), 'spawnAgent').mockRejectedValue(new Error('Login required'));
    render(<AttentionList />);
    expect(screen.getAllByText('Web repository')).toHaveLength(2);
    expect(screen.queryByText('Healthy')).toBeNull();
    expect(screen.getByRole('button', { name: 'Retry Lost-report' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Retry Failed-build' }));
    await waitFor(() => expect(recovery).toHaveBeenCalledWith(2, 'claude'));
    const { useToastStore } = await import('../../src/stores/toastStore');
    await waitFor(() => expect(useToastStore.getState().toasts.some(t => t.message === 'Login required')).toBe(true));
  });

  it.each(['mesh', 'pinned', 'filtered'] as const)('Attention reveals a cross-repository terminal from %s mode', mode => {
    seedAgentNodes([node(1, 'API waiting', 'awaiting_input'), node(2, 'Web failure', 'error', 2)]);
    useMeshStore.setState({ selectedMeshId: 1 }); useUIStore.getState().setViewMode(mode);
    useUIStore.getState().setGridSearchQuery('unmatched');
    render(<AttentionList />);
    fireEvent.click(screen.getByText('Web failure').closest('button')!);
    expect(useMeshStore.getState().selectedMeshId).toBe(2);
    expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
    expect(useUIStore.getState().viewMode).toBe('single');
  });

  it('the readiness guide can be skipped and restored, and Terminal stays actionable without a harness', () => {
    vi.mocked(invoke).mockResolvedValue([]);
    const terminal = vi.fn(); const setup = vi.fn();
    render(<ReadinessSteps repositoryReady harnessReady={false} callbacks={{ onCreateMesh: vi.fn(), onOpenSpawnMenu: vi.fn(), onClearFilters: vi.fn(), onOpenSetup: setup, onViewAll: vi.fn(), onOpenTerminal: terminal }} />);
    fireEvent.click(screen.getByRole('button', { name: 'Start Terminal' })); expect(terminal).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button', { name: 'Check runtime and login' })); expect(setup).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button', { name: 'Skip setup guide' }));
    expect(screen.queryByRole('list', { name: 'Getting started' })).toBeNull();
    expect(localStorage.getItem('buildmesh.readiness-dismissed')).toBe('true');
    fireEvent.click(screen.getByRole('button', { name: 'Show setup guide' }));
    expect(screen.getAllByRole('listitem')).toHaveLength(3);
  });

  it('history includes archived work across repositories, filters by lifecycle and reopens without spawning', async () => {
    const archived = node(4, 'Old-web-work', 'archived', 2);
    vi.mocked(invoke).mockImplementation(cmd => Promise.resolve(cmd === 'list_agent_history' ? [archived, node(3, 'API-error', 'error')] : cmd === 'reopen_agent_node' ? { ...archived, status: 'suspended' } : []));
    const fetch = vi.spyOn(useAgentNodeStore.getState(), 'fetchAgentNodes').mockRejectedValue(new Error('Workspace refresh failed'));
    const spawn = vi.spyOn(useAgentNodeStore.getState(), 'spawnAgent').mockResolvedValue(undefined as never);
    const activate = vi.spyOn(useNodeActivityStore.getState(), 'activateNode').mockImplementation(() => {});
    render(<AgentHistoryTab />);
    expect(await screen.findByText('Old-web-work')).toBeTruthy();
    fireEvent.change(screen.getByLabelText('History status'), { target: { value: 'archived' } });
    expect(screen.queryByText('API-error')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Reopen' }));
    await waitFor(() => expect(activate).toHaveBeenCalledWith(4));
    expect(vi.mocked(invoke)).toHaveBeenCalledWith('reopen_agent_node', { nodeId: 4 });
    expect(useAgentNodeStore.getState().nodesById[4]).toMatchObject({ id: 4, status: 'suspended', cli_session_id: 'saved-session' });
    expect(fetch).not.toHaveBeenCalled(); expect(spawn).not.toHaveBeenCalled();
  });
});
