import { act, cleanup, fireEvent, render, renderHook, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { GridControls } from '../../src/components/TitleBar/GridControls';
import { AttentionList } from '../../src/components/Sidebar/AttentionList';
import { AgentHistoryTab } from '../../src/components/Probe/AgentHistoryTab';
import { ReadinessSteps } from '../../src/components/AgentNodeView/ReadinessSteps';
import { CanvasEmptyStateContainer } from '../../src/components/AgentNodeView/CanvasEmptyStateContainer';
import { deriveScope } from '../../src/lib/viewModes';
import { useSettingsResources } from '../../src/components/AppSettings/useSettingsResources';
import { FileTree } from '../../src/components/FileTree/FileTree';
import { useUIStore, type ViewMode } from '../../src/stores/uiStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useNodeActivityStore } from '../../src/stores/nodeActivityStore';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';
import { __resetSharedProviderListForTests } from '../../src/hooks/useProviderList';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';
import type { LaunchTarget } from '../../src/types/generated/LaunchTarget';
import type { SpawnConfiguration } from '../../src/types/generated/SpawnConfiguration';
import { HARNESS_CAPABILITIES } from '../../src/types/generated/HarnessCapabilitiesTable';

vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.0.0') }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const meshA = { id: 1, name: 'API repository', path: '/api', base_ref: 'main', use_worktree: true };
const meshB = { id: 2, name: 'Web repository', path: '/web', base_ref: 'main' };
function node(id: number, name: string, status: AgentNode['status'], meshId = 1): AgentNode {
  return { id, mesh_id: meshId, name, status, path: '/api', branch: 'main', env: 'windows', provider: 'claude', use_worktree: false, created_at: '2026-10-02', cli_session_id: 'saved-session' } as AgentNode;
}
const route = { id: 'claude', label: 'Claude Code', harness_id: 'claude', group_key: 'claude', provider_id: null, is_proxied: false, color: '#fff', icon: '', resumable: true, capabilities: HARNESS_CAPABILITIES.anthropic } as ProviderInfo;
function mockSettings(providers: Promise<ProviderInfo[]>, prefs = Promise.resolve({ default_provider: 'claude', reviewer_provider: null, naming_provider: null, harness_defaults: {}, provider_pairings: [] })) {
  vi.mocked(invoke).mockImplementation(cmd => {
    if (cmd === 'get_app_preferences') return prefs;
    if (cmd === 'list_routing_options') return Promise.resolve([route]);
    if (cmd === 'list_providers') return providers;
    if (cmd === 'get_coordinator_status') return Promise.resolve({ enabled: false, has_token: false });
    if (cmd === 'get_network_status') return Promise.resolve({ lan_exposure_enabled: false, tls_active: false, exposed_interfaces: [] });
    if (['get_provider_accounts', 'get_keyed_first_class_catalog', 'get_provider_pairings', 'get_pairing_verifications', 'list_device_sessions', 'list_spawn_configurations'].includes(cmd)) return Promise.resolve([]);
    return Promise.resolve({});
  });
}
beforeEach(() => {
  localStorage.clear();
  __resetProviderCachesForTests(); __resetSharedProviderListForTests();
  useUIStore.getState().resetGridControls();
  useMeshStore.setState({ selectedMeshId: 1, meshes: [meshA, meshB] as never, meshesById: new Map([[1, meshA], [2, meshB]]) as never });
  seedAgentNodes([]);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

// #2071 — the container renders the scope `AgentNodeView` derives; these
// tests build the same one for a hand-set canvas state.
const emptyScope = (viewMode: ViewMode, selectedMeshId: number | null, agentNodes: AgentNode[]) =>
  deriveScope({ viewMode, lastNonSingleMode: viewMode === 'single' ? 'all' : viewMode, agentNodes, selectedMeshId, activeNodeId: null });

describe('October desktop audit follow-up', () => {
  it('a skipped guide stays optional on another empty repository while its Terminal and restore actions remain available', () => {
    localStorage.setItem('buildmesh.readiness-dismissed', 'true');
    vi.mocked(invoke).mockReturnValue(new Promise(() => {}));
    const elsewhere = [node(20, 'Other repository work', 'running', 2)];
    render(<CanvasEmptyStateContainer viewMode="mesh" selectedMeshId={1} agentNodes={elsewhere} scope={emptyScope('mesh', 1, elsewhere)} />);
    expect(screen.queryByRole('list', { name: 'Getting started' })).toBeNull();
    expect((screen.getByRole('button', { name: 'Start Terminal' }) as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(screen.getByRole('button', { name: 'Show setup guide' }));
    expect(within(screen.getByRole('list', { name: 'Getting started' })).getAllByRole('listitem')).toHaveLength(3);
  });
  it('an initial history failure reports unavailable until Retry returns real rows', async () => {
    let reads = 0;
    vi.mocked(invoke).mockImplementation(cmd => cmd === 'list_agent_history'
      ? ++reads === 1 ? Promise.reject(new Error('Database unavailable')) : Promise.resolve([node(5, 'Recovered history', 'error')])
      : Promise.resolve([]));
    render(<AgentHistoryTab />);
    expect((await screen.findByRole('alert')).textContent).toContain('Database unavailable');
    expect(screen.queryByText('No agent history yet.')).toBeNull();
    expect(screen.getByText('Agent history unavailable.')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Retry history' }));
    expect(await screen.findByText('Recovered history')).toBeTruthy();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('an empty selected repository keeps Terminal available while another repository has agents and checks fail', async () => {
    const checks = deferred<ProviderInfo[]>();
    const retry = deferred<ProviderInfo[]>();
    let reads = 0;
    vi.mocked(invoke).mockImplementation(cmd => cmd === 'list_providers' ? (++reads === 1 ? checks.promise : retry.promise) : Promise.resolve([]));
    const elsewhere = [node(20, 'Other repository work', 'running', 2)];
    seedAgentNodes(elsewhere);
    render(<CanvasEmptyStateContainer viewMode="mesh" selectedMeshId={1} agentNodes={elsewhere} scope={emptyScope('mesh', 1, elsewhere)} />);
    expect(screen.getByRole('button', { name: 'Start Terminal' })).toBeTruthy();
    expect(screen.getByRole('status').textContent).toContain('Checking available harnesses');
    fireEvent.click(screen.getByRole('button', { name: 'Check runtime and login' }));
    expect(useUIStore.getState()).toMatchObject({ appSettingsOpen: true, appSettingsTab: 'providers' });
    await act(async () => checks.reject(new Error('Runtime check failed')));
    expect((await screen.findByRole('alert')).textContent).toContain('Runtime check failed');
    fireEvent.click(screen.getByRole('button', { name: 'Retry harness check' }));
    expect(screen.getByRole('status').textContent).toContain('Checking available harnesses');
    expect((screen.getByRole('button', { name: 'Start Terminal' }) as HTMLButtonElement).disabled).toBe(false);
    await act(async () => retry.resolve([route]));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    expect(screen.getByText(/A harness is available/)).toBeTruthy();
    expect(reads).toBe(2);
  });

  it('opens the requested Settings pane and defaults normal opens to General', async () => {
    mockSettings(Promise.resolve([route]));
    const { unmount } = render(<AppSettingsModal onClose={vi.fn()} initialTab="providers" />);
    expect(screen.getByRole('tab', { name: 'Providers' }).getAttribute('aria-selected')).toBe('true');
    unmount();
    useUIStore.getState().openAppSettings();
    expect(useUIStore.getState()).toMatchObject({ appSettingsTab: 'general' });
  });

  it('saving and deleting a Launch Configuration refreshes both catalogs while live probes remain pending', async () => {
    const target: LaunchTarget = { id: 'claude', harness_id: 'claude', harness_name: 'Claude Code', provider_name: 'Native authentication', models: [], efforts: [], route_attached: false, manual_model: false, supports_model: false, supports_extra_args: true };
    let saved: SpawnConfiguration | null = null;
    let routingReads = 0;
    let liveReads = 0;
    const pending = deferred<ProviderInfo[]>();
    vi.mocked(invoke).mockImplementation((cmd, args) => {
      if (cmd === 'get_app_preferences') return Promise.resolve({ default_provider: 'launch/audit', reviewer_provider: null, naming_provider: null, harness_defaults: {}, provider_pairings: [] });
      if (cmd === 'list_routing_options') { routingReads++; return Promise.resolve(saved ? [{ ...route, configurations: [saved] }, { ...route, id: saved.id, label: saved.name, configuration: saved }] : [route]); }
      if (cmd === 'list_providers') { liveReads++; return pending.promise; }
      if (cmd === 'get_launch_targets') return Promise.resolve([target]);
      if (cmd === 'list_spawn_configurations') return Promise.resolve(saved ? [saved] : []);
      if (cmd === 'save_spawn_configuration') { saved = { ...(args as { value: SpawnConfiguration }).value, id: 'launch/audit' }; return Promise.resolve(saved); }
      if (cmd === 'delete_spawn_configuration') { saved = null; return Promise.resolve(); }
      if (cmd === 'get_network_status') return Promise.resolve({ lan_exposure_enabled: false, tls_active: false, exposed_interfaces: [] });
      if (cmd === 'get_coordinator_status') return Promise.resolve({ enabled: false, has_token: false });
      return Promise.resolve([]);
    });
    render(<AppSettingsModal onClose={vi.fn()} />);
    fireEvent.click(screen.getByRole('tab', { name: 'Launch Configurations' }));
    await waitFor(() => expect((screen.getByRole('button', { name: 'New Launch Configuration' }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole('button', { name: 'New Launch Configuration' }));
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Audit recipe' } });
    const form = screen.getByLabelText('Name').closest('form')!;
    fireEvent.click(within(form).getByRole('button', { name: 'Save' }));
    await screen.findByRole('button', { name: 'Edit Audit recipe' });
    await waitFor(() => expect(routingReads).toBe(2));
    expect(liveReads).toBe(2);
    fireEvent.click(screen.getByRole('tab', { name: 'Providers' }));
    await waitFor(() => expect(screen.getByLabelText('Default provider').textContent).toContain('Audit recipe'));
    fireEvent.click(screen.getByRole('tab', { name: 'Launch Configurations' }));
    fireEvent.click(screen.getByRole('button', { name: 'Edit Audit recipe' }));
    fireEvent.click(within(screen.getByLabelText('Name').closest('form')!).getByRole('button', { name: 'Delete' }));
    fireEvent.click(within(screen.getByRole('dialog', { name: 'Delete Launch Configuration?' })).getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(routingReads).toBe(3));
    expect(liveReads).toBe(3);
    fireEvent.click(screen.getByRole('tab', { name: 'Providers' }));
    await waitFor(() => expect(screen.getByLabelText('Default provider').textContent).not.toContain('Audit recipe'));
  });

  it('Escape closes filters while focus is still on their trigger', () => {
    render(<GridControls />);
    const trigger = screen.getByRole('button', { name: /Filters and sort:/ });
    trigger.focus(); fireEvent.click(trigger);
    expect(screen.getByRole('region', { name: 'Filters and sort' })).toBeTruthy();
    fireEvent.keyDown(trigger, { key: 'Escape' });
    expect(screen.queryByRole('region', { name: 'Filters and sort' })).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
  it('Settings has one tabbable tab, wraps vertical keys and wires panels', async () => {
    mockSettings(Promise.resolve([route]));
    render(<AppSettingsModal onClose={vi.fn()} />);
    // Derived, not hard-coded to a named tab: this asserts that `ArrowUp`
    // from the first tab wraps to the *last* one, which is the behaviour.
    // Pinning "Remote Access" re-breaks every time the Settings modal gains a
    // tab (issue #1537 added "Data & Diagnostics").
    const tabs = screen.getAllByRole('tab');
    const last = tabs[tabs.length - 1];
    const general = screen.getByRole('tab', { name: 'General' });
    general.focus(); fireEvent.keyDown(general, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(last);
    expect(last.getAttribute('aria-selected')).toBe('true');
    expect(screen.getAllByRole('tab').filter(t => t.tabIndex === 0)).toEqual([last]);
    const panel = document.getElementById(last.getAttribute('aria-controls')!);
    expect(panel?.getAttribute('aria-labelledby')).toBe(last.id);
    expect(panel?.hidden).toBe(false);
    fireEvent.keyDown(last, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(general);
    fireEvent.keyDown(general, { key: 'End' }); expect(document.activeElement).toBe(last);
    fireEvent.keyDown(last, { key: 'Home' }); expect(document.activeElement).toBe(general);
    await act(async () => {});
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
  });

  it('failed preferences keep routing pickers disabled even when both choice sources succeed', async () => {
    const prefs = deferred<{ default_provider: string; reviewer_provider: null; naming_provider: null; harness_defaults: object; provider_pairings: never[] }>();
    mockSettings(Promise.resolve([route]), prefs.promise);
    render(<AppSettingsModal onClose={vi.fn()} />);
    await act(async () => prefs.reject(new Error('Preferences unreadable')));
    fireEvent.click(screen.getByRole('tab', { name: 'Providers' }));
    for (const label of ['Default provider', 'Circuit classifier provider', 'Reviewer provider', 'Auto-naming']) {
      const picker = screen.getByLabelText(label) as HTMLButtonElement;
      expect(picker.disabled).toBe(true);
      fireEvent.click(picker);
      expect(screen.queryByTestId('spawn-option-picker-menu')).toBeNull();
    }
    expect(screen.getAllByTestId('resource-load-preferences')[0].textContent).toContain('Preferences unreadable');
    const restored = { default_provider: 'claude', reviewer_provider: null, naming_provider: null, harness_defaults: {}, provider_pairings: [] };
    vi.mocked(invoke).mockImplementation(cmd => Promise.resolve(cmd === 'get_app_preferences' ? restored : cmd === 'list_providers' || cmd === 'list_routing_options' ? [route] : []));
    fireEvent.click(screen.getByRole('button', { name: 'Retry loading preferences' }));
    for (const [label, command] of [
      ['Default provider', 'set_app_default_provider'],
      ['Circuit classifier provider', 'set_circuit_classifier_provider'],
      ['Reviewer provider', 'set_app_reviewer_provider'],
      ['Auto-naming', 'set_app_naming_provider'],
    ]) {
      const picker = screen.getByLabelText(label) as HTMLButtonElement;
      await waitFor(() => expect(picker.disabled).toBe(false));
      fireEvent.click(picker);
      fireEvent.click(screen.getByRole('menuitem', { name: 'Claude Code' }));
      await waitFor(() => expect(invoke).toHaveBeenCalledWith(command, expect.objectContaining({ provider: 'claude' })));
    }
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
    vi.mocked(invoke).mockImplementation(cmd => cmd === 'create_agent_node' ? creating.promise : Promise.resolve([]));
    render(<CanvasEmptyStateContainer viewMode="all" selectedMeshId={1} agentNodes={[]} scope={emptyScope('all', 1, [])} />);
    const start = screen.getByRole('button', { name: 'Start Terminal' });
    fireEvent.click(start); fireEvent.click(start);
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'create_agent_node')).toHaveLength(1);
    expect(invoke).toHaveBeenCalledWith('create_agent_node', expect.objectContaining({ meshId: 1, provider: 'terminal', useWorktree: undefined }));
    expect((start as HTMLButtonElement).disabled).toBe(true);
    await act(async () => creating.reject(new Error('Repository unavailable')));
    await waitFor(() => expect((start as HTMLButtonElement).disabled).toBe(false));
    const { useToastStore } = await import('../../src/stores/toastStore');
    expect(useAgentNodeStore.getState().error).toBe('Repository unavailable');
    expect(useToastStore.getState().toasts.filter(t => t.message === 'Repository unavailable')).toHaveLength(0);
    vi.mocked(invoke).mockResolvedValue({ ...node(8, 'Terminal', 'idle'), use_worktree: true, worktree_path: '/api-worktree' });
    fireEvent.click(start);
    await waitFor(() => expect(useAgentNodeStore.getState().nodesById[8]?.worktree_path).toBe('/api-worktree'));
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'create_agent_node')).toHaveLength(2);
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

  it('the readiness guide can be skipped and restored, and Terminal stays actionable without a harness', async () => {
    vi.mocked(invoke).mockResolvedValue([]);
    const terminal = vi.fn(); const setup = vi.fn();
    render(<ReadinessSteps repositoryReady harnessReady={false} callbacks={{ onCreateMesh: vi.fn(), onOpenSpawnMenu: vi.fn(), onClearFilters: vi.fn(), onOpenSetup: setup, onViewAll: vi.fn(), onOpenTerminal: terminal }} />);
    fireEvent.click(screen.getByRole('button', { name: 'Start Terminal' })); expect(terminal).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button', { name: 'Check runtime and login' })); expect(setup).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button', { name: 'Skip setup guide' }));
    expect(screen.queryByRole('list', { name: 'Getting started' })).toBeNull();
    expect(localStorage.getItem('buildmesh.readiness-dismissed')).toBe('true');
    fireEvent.click(screen.getByRole('button', { name: 'Show setup guide' }));
    expect(within(screen.getByRole('list', { name: 'Getting started' })).getAllByRole('listitem')).toHaveLength(3);
    await act(async () => {});
  });

  it.each(['empty', 'failure'] as const)('history keeps a reopened row after a subsequent %s read, without spawning', async outcome => {
    const archived = node(4, 'Old-web-work', 'archived', 2);
    let reads = 0;
    vi.mocked(invoke).mockImplementation(cmd => {
      if (cmd === 'list_agent_history') return ++reads === 1 ? Promise.resolve([archived, node(3, 'API-error', 'error')])
        : outcome === 'empty' ? Promise.resolve([]) : Promise.reject(new Error('History refresh failed'));
      return Promise.resolve(cmd === 'reopen_agent_node' ? { ...archived, status: 'suspended' } : []);
    });
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
    fireEvent.change(screen.getByLabelText('History status'), { target: { value: '' } });
    await waitFor(() => expect(reads).toBe(2));
    if (outcome === 'failure') expect((await screen.findByRole('alert')).textContent).toContain('History refresh failed');
    else await waitFor(() => expect(screen.queryByText('API-error')).toBeNull());
    expect(screen.getByText('Old-web-work')).toBeTruthy();
    expect(within(screen.getByText('Old-web-work').parentElement!).getByText('Suspended')).toBeTruthy();
    expect(spawn).not.toHaveBeenCalled();
  });
});
