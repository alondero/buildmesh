// Header identity, selected-session actions, recovery, and PR interactions.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

// Irrelevant to chip styling — stub it.
vi.mock('../../src/hooks/useProviderList', () => ({ useProviderList: () => [], __resetSharedProviderListForTests: () => {} }));
vi.mock('../../src/components/BuildRun/BuildRunDropdown', () => ({
  BuildRunDropdown: () => null,
}));

// Each test sets what the chip will see by calling summaryMock.mockReturnValue(...).
const summaryMock = vi.fn();
vi.mock('../../src/hooks/useGitSummary', () => ({
  useGitSummary: () => ({ summary: summaryMock(), loading: false, refresh: vi.fn() }),
}));

// Each test sets what the PR chip will see by calling prMock.mockReturnValue(...)
// (or .mockReturnValue(null) to hide the chip). Mirrors the summaryMock pattern.
const prMock = vi.fn();
const invalidateOpenPrForNodeMock = vi.fn();
vi.mock('../../src/hooks/useOpenPr', () => ({
  useOpenPr: () => ({ pr: prMock(), loading: false, refresh: vi.fn() }),
  invalidateOpenPrForNode: (...args: unknown[]) => invalidateOpenPrForNodeMock(...args),
}));

const telemetryMock = vi.fn(() => null);
vi.mock('../../src/hooks/useMuseSessionTelemetry', () => ({
  useMuseSessionTelemetry: (...args: unknown[]) => telemetryMock(...args),
}));

// Stub the opener plugin so the chip's onClick doesn't try to launch a real browser.
// `vi.hoisted` is required because `vi.mock` factories are hoisted to the top of
// the file, before any `const` declarations at module scope.
const { openUrlMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: openUrlMock,
}));

// Stub the openInFileManager IPC wrapper. Spread the real module via
// `importOriginal` so any other named export added to lib/tauri in the
// future (none used by GridNodeHeader today) keeps working — we only
// pin this one call. The IPC failure modes the command returns (path
// missing / not a directory) are exercised in src-tauri's own tests;
// here we just assert wiring: the click resolves the path through
// `getNodeGitPath` and hands it to the IPC layer.
const { openInFileManagerMock, toggleNodePinnedMock, mergePrMock } = vi.hoisted(() => ({
  openInFileManagerMock: vi.fn().mockResolvedValue(undefined),
  toggleNodePinnedMock: vi.fn(),
  mergePrMock: vi.fn().mockResolvedValue('Merged'),
}));
vi.mock('../../src/lib/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/tauri')>();
  return {
    ...actual,
    openInFileManager: openInFileManagerMock,
    toggleNodePinned: toggleNodePinnedMock,
    mergePr: mergePrMock,
  };
});

import { GridNodeHeader } from '../../src/components/AgentNodeView/GridNodeHeader';
import { seedAgentNodes } from './helpers/seedAgentNodes';

const NODE: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'anthropic',
  status: 'running',
  use_worktree: false,
  position: 0,
  created_at: new Date(0).toISOString(),
  scratchpad: '',
  sandbox: false,
  is_pinned: false,
};

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'single',
  position: 0,
  created_at: new Date(0).toISOString(),
};


describe('GridNodeHeader contextual information and actions', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useAgentNodeStore.setState({ autopilotStates: {}, circuitOwnerships: {} });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ viewMode: 'mesh', probeOpen: false, probeTab: 'files', pendingCircuitRunFocus: null });
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    prMock.mockReturnValue(null);
    telemetryMock.mockReturnValue(null);
    openInFileManagerMock.mockClear();
  });

  it('keeps metadata available on demand without repeating it in the title', () => {
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const header = screen.getByTestId('grid-node-header');
    expect(header.textContent).not.toContain('Repository root');
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    const menu = screen.getByRole('menu', { name: 'Agent node actions' });
    expect(menu).toBeTruthy();
    const details = screen.getByTestId('grid-node-details');
    expect(details.textContent).toContain('demo');
    expect(details.textContent).toContain('Repository root');
    expect(details.textContent).toContain('6 changed files');
    fireEvent.click(screen.getByRole('menuitem', { name: 'Agent node details' }));
    expect(useUIStore.getState().probeTab).toBe('properties');
    expect(useUIStore.getState().probeOpen).toBe(true);
  });

  it('keeps the task title stable while actions and details target the selected reviewer', async () => {
    const reviewer = { ...NODE, id: 2, name: 'Security reviewer', path: '/review-worktree' };
    seedAgentNodes([NODE, reviewer], NODE.id);
    const deleteAgentNode = vi.fn().mockResolvedValue(undefined);
    useAgentNodeStore.setState({ deleteAgentNode });
    render(<GridNodeHeader nodeId={2} titleNodeId={1} onBuildRun={() => {}} />);
    expect(screen.getByTestId('grid-node-header').textContent).toContain('agent-1');
    expect(screen.getByTestId('grid-node-header').textContent).not.toContain('Security reviewer');
    fireEvent.click(screen.getByRole('button', { name: 'Close agent node' }));
    await waitFor(() => expect(deleteAgentNode).toHaveBeenCalledWith(2));
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'Open in file explorer' }));
    expect(openInFileManagerMock).toHaveBeenCalledWith('/review-worktree');
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'View changes' }));
    expect(useAgentNodeStore.getState().activeNodeId).toBe(2);
    expect(useUIStore.getState().probeTab).toBe('review');
  });

  it('pins the selected session through the existing store action', async () => {
    toggleNodePinnedMock.mockResolvedValue({ ...NODE, is_pinned: true });
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'Pin node' }));
    await waitFor(() => expect(toggleNodePinnedMock).toHaveBeenCalledWith(NODE.id));
    expect(useAgentNodeStore.getState().nodesById[NODE.id].is_pinned).toBe(true);
  });

  it('exposes circuit ownership in session details', () => {
    useAgentNodeStore.setState({ circuitOwnerships: { 1: { node_id: 1, run_id: 2, circuit_id: 9,
      circuit_name: 'Review workflow', state: 'running', parent_node_id: null } } });
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(screen.queryByTestId('circuit-run-pill')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    const pill = screen.getByTestId('circuit-run-pill');
    expect(pill.textContent).toBe('Review workflow · #2');
    expect(pill.className).toContain('text-accent-violet');
    expect(pill.getAttribute('title')).toContain('Autopilot is driving');
  });

  it('links circuit ownership to the run in the Circuits Probe', () => {
    useAgentNodeStore.setState({ circuitOwnerships: { 1: { node_id: 1, run_id: 2, circuit_id: 9,
      circuit_name: 'Review workflow', state: 'running', parent_node_id: null } } });
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    fireEvent.click(screen.getByTestId('circuit-run-pill'));
    expect(useUIStore.getState().probeTab).toBe('circuits');
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().pendingCircuitRunFocus).toBe(2);
  });

  it.each([
    ['failed', 'text-status-error'],
    ['cancelled', 'text-accent-amber'],
  ] as const)('keeps Circuit %s history richly styled in session details', (state, toneClass) => {
    useAgentNodeStore.setState({ circuitOwnerships: { 1: { node_id: 1, run_id: 2, circuit_id: 9,
      circuit_name: 'Review workflow', state, parent_node_id: null } } });
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    const pill = screen.getByTestId('circuit-run-pill');
    expect(pill.className).toContain(toneClass);
    expect(pill.getAttribute('title')).toContain(state);
  });

  it('keeps session and signal health warnings distinct at compact widths', () => {
    seedAgentNodes([{ ...NODE, status: 'suspended', cli_session_id: null, signal_health: 'unavailable' }], NODE.id);
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    expect(screen.getByText('No session')).toBeTruthy();
    expect(screen.getByRole('img', { name: 'Attention signal unavailable' })).toBeTruthy();
  });

  it.each([
    ['implementing', 'autopilot'],
    ['finishing', 'autopilot · wrap-up'],
    ['suffix_pending', 'autopilot · suffix'],
    ['completed', 'autopilot · complete'],
    ['merged', 'autopilot · merged'],
    ['failed', 'autopilot ✗'],
  ] as const)('keeps the %s Autopilot status visible in the details menu with its tooltip', (state, label) => {
    useAgentNodeStore.setState({ autopilotStates: { 1: state } });
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    const pill = screen.getByTestId('autopilot-pill');
    expect(pill.textContent).toContain(label);
    expect(pill.getAttribute('title')).toContain('Autopilot:');
    expect(pill.className).toContain('ring-');
  });

  it('keeps git summary additions, modifications, and deletions semantically coloured in details', () => {
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    expect(screen.getByText('+3').className).toContain('text-accent-green');
    expect(screen.getByText('~2').className).toContain('text-accent-amber');
    expect(screen.getByText('-1').className).toContain('text-accent-red');
  });

  it('keeps aggregate attention actionable even when its session tab is offscreen', () => {
    const onAttention = vi.fn();
    render(<GridNodeHeader nodeId={NODE.id} activity={{ label: 'Needs input', tone: 'warning' }}
      attentionCount={2} onAttention={onAttention} onBuildRun={() => {}} />);
    expect(screen.getByRole('status', { name: 'Needs input' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: /2 sessions need attention/ }));
    expect(onAttention).toHaveBeenCalledOnce();
  });

  it('reserves the ownership cell without rendering an unpiloted indicator', () => {
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const cell = screen.getByTestId('autopilot-indicator-cell');
    expect(cell.className).toContain('w-3.5');
    expect(cell.querySelector('[data-testid="autopilot-indicator"]')).toBeNull();
  });

  it('renders the shared active indicator for a driving legacy Autopilot run', () => {
    useAgentNodeStore.setState({ autopilotStates: { 1: 'implementing' } });
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const indicator = screen.getByRole('img', { name: 'Autopilot active' });
    expect(indicator).toBeTruthy();
    expect(indicator.getAttribute('title')).toBe('Autopilot is driving this Agent Node.');
    expect(indicator.querySelector('svg')?.getAttribute('class')).toContain('motion-reduce:animate-none');
  });

  it('renders Circuit terminal ownership as Done', () => {
    useAgentNodeStore.setState({ circuitOwnerships: { 1: { node_id: 1, run_id: 2, circuit_id: 9,
      circuit_name: 'Review workflow', state: 'completed', parent_node_id: null } } });
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(screen.getByRole('img', { name: 'Autopilot done' })).toBeTruthy();
  });

  it('renders waiting and failure tones without changing the ownership cell', () => {
    seedAgentNodes([{ ...NODE, status: 'awaiting_input' }], NODE.id);
    useAgentNodeStore.setState({ autopilotStates: { 1: 'finishing' } });
    const { rerender } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(screen.getByRole('img', { name: 'Autopilot waiting' })).toBeTruthy();

    useAgentNodeStore.setState({ autopilotStates: { 1: 'failed' } });
    rerender(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(screen.getByRole('img', { name: 'Autopilot needs attention' })).toBeTruthy();
  });

  it('keeps cancelled Circuit history out of the compact indicator', () => {
    useAgentNodeStore.setState({ circuitOwnerships: { 1: { node_id: 1, run_id: 2, circuit_id: 9,
      circuit_name: 'Review workflow', state: 'cancelled', parent_node_id: null } } });
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(screen.getByTestId('autopilot-indicator-cell').querySelector('[data-testid="autopilot-indicator"]')).toBeNull();
  });
});
describe('GridNodeHeader solo view (#65; View Modes wayfinder #982)', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    // Single subsumes the old per-node maximize: "this header is the solo
    // view's" is now "the canvas is in Single mode". Baseline is a grid mode.
    useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue(null);
  });

  it('double-clicking the header enters Single on this node', () => {
    const { container } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    // The header root carries the double-click handler.
    fireEvent.doubleClick(container.firstChild as Element);
    expect(useUIStore.getState().viewMode).toBe('single');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(NODE.id);
  });

  it('double-clicking again restores the grid mode Single came from', () => {
    const { container } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireEvent.doubleClick(container.firstChild as Element);
    fireEvent.doubleClick(container.firstChild as Element);
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });

  it('the explicit button toggles Single and flips its label', () => {
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireEvent.click(getByLabelText('Maximize agent node'));
    expect(useUIStore.getState().viewMode).toBe('single');
    // Once soloed, the same control offers "restore".
    fireEvent.click(getByLabelText('Restore grid layout'));
    expect(useUIStore.getState().viewMode).toBe('mesh');
  });

  // Issue #668 — Alt+G (Win/Linux) / Cmd+G (macOS) is the keyboard
  // counterpart to the double-click and the explicit button. The header
  // tooltip must surface it so users discover the shortcut without
  // hunting through the empty-state splash.
  it('mentions the platform-specific Alt+G / ⌥+G shortcut in the maximize tooltip', () => {
    const { container } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const title = (container.firstChild as Element).getAttribute('title') ?? '';
    // Canonical sentence — substring-only asserts would accept broken
    // strings like "Alt+G to do something unrelated maximize".
    const isMac = navigator.platform.toUpperCase().includes('MAC');
    const expected = isMac
      ? 'Double-click or press ⌥+G to maximize'
      : 'Double-click or press Alt+G to maximize';
    expect(title).toBe(expected);
  });

  it('mentions the shortcut in the restore tooltip when the node is soloed', () => {
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    const { container } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const title = (container.firstChild as Element).getAttribute('title') ?? '';
    const isMac = navigator.platform.toUpperCase().includes('MAC');
    const expected = isMac
      ? 'Double-click or press ⌥+G to restore grid'
      : 'Double-click or press Alt+G to restore grid';
    expect(title).toBe(expected);
  });

  // Issue #668 — the explicit maximize button (visible on hover) must
  // advertise the same shortcut so discoverability isn't gated on the
  // header-bar double-click affordance.
  it('the maximize button tooltip mentions the Alt+G / ⌥+G shortcut', () => {
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const button = getByLabelText('Maximize agent node');
    const title = button.getAttribute('title') ?? '';
    const isMac = navigator.platform.toUpperCase().includes('MAC');
    expect(title).toMatch(isMac ? /⌥\+G/ : /Alt\+G/);
    expect(title.toLowerCase()).toContain('maximize');
  });

  it('the restore button tooltip mentions the shortcut when soloed', () => {
    useUIStore.setState({ viewMode: 'single', lastNonSingleMode: 'mesh' });
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const button = getByLabelText('Restore grid layout');
    const title = button.getAttribute('title') ?? '';
    const isMac = navigator.platform.toUpperCase().includes('MAC');
    expect(title).toMatch(isMac ? /⌥\+G/ : /Alt\+G/);
    expect(title.toLowerCase()).toContain('restore');
  });
});

describe('GridNodeHeader PR chip', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    summaryMock.mockReset();
    prMock.mockReset();
    openUrlMock.mockClear();
  });

  it('renders "PR #123" when an open PR exists for the branch', () => {
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue({
      number: 123,
      url: 'https://github.com/alondero/buildmesh/pull/123',
      title: 'Add PR chip',
      draft: false,
    });
    const { getByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const chip = getByText('PR #123');

    // Green "open" semantic (matches the rest of the chip family)
    expect(chip.className).toContain('text-accent-green');
    expect(chip.className).toContain('cursor-pointer');
    // Titlebar chip pattern
    expect(chip.className).toContain('rounded-full');
    expect(chip.className).toContain('ring-1');
  });

  it('hides the chip when no open PR exists', () => {
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue(null);
    const { queryByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(queryByText(/^PR #/)).toBeNull();
  });

  it('clicking the pill opens a menu instead of the browser directly', () => {
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue({
      number: 123,
      url: 'https://github.com/alondero/buildmesh/pull/123',
      title: 'Add PR chip',
      draft: false,
    });
    const { getByText, getByRole } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    fireEvent.click(getByText('PR #123'));
    // Menu replaces the direct open — browser stays shut until the user
    // picks "Open on GitHub" inside the menu.
    expect(openUrlMock).not.toHaveBeenCalled();
    expect(getByRole('menu')).toBeTruthy();
    expect(getByText(/Open on GitHub/)).toBeTruthy();
    expect(getByText(/Merge \(squash/)).toBeTruthy();
  });

  it('menu "Open on GitHub" opens the PR in the default browser', () => {
    summaryMock.mockReturnValue(null);
    const url = 'https://github.com/alondero/buildmesh/pull/123';
    prMock.mockReturnValue({ number: 123, url, title: 'Add PR chip', draft: false });
    const { getByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    fireEvent.click(getByText('PR #123'));
    fireEvent.click(getByText(/Open on GitHub/));
    expect(openUrlMock).toHaveBeenCalledWith(url);
  });

  it('menu Merge requires confirm then calls mergePr', async () => {
    summaryMock.mockReturnValue(null);
    const url = 'https://github.com/alondero/buildmesh/pull/123';
    prMock.mockReturnValue({ number: 123, url, title: 'Add PR chip', draft: false });
    mergePrMock.mockClear();
    const { getByText, getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    fireEvent.click(getByText('PR #123'));
    fireEvent.click(getByText(/Merge \(squash/));
    // First click arms confirm — must NOT merge yet.
    expect(mergePrMock).not.toHaveBeenCalled();

    fireEvent.click(getByLabelText('Confirm squash merge of pull request #123'));
    const { waitFor } = await import('@testing-library/react');
    await waitFor(() => {
      expect(mergePrMock).toHaveBeenCalledWith(url);
    });
  });

  it('opens the probe panel on the review tab when the git-summary chip is clicked (#376)', () => {
    // Issue #376 — clicking the +/~/- chip opens the unified Probe Panel
    // on the ðŸ” tab so the user lands on the active agent's
    // Agent Changes list.
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    prMock.mockReturnValue(null);
    useUIStore.setState({
      probeOpen: false,
      probeTab: 'files',
    });
    const { getByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'View changes' }));

    const state = useUIStore.getState();
    expect(state.probeOpen).toBe(true);
    expect(state.probeTab).toBe('review');
  });

  it('focuses this node when the chip is clicked, so the review tab shows THIS node\'s diff', () => {
    // Regression guard: the new flow routes through `AgentChangesTab`
    // which reads `useProbeContext().activeNodeId`. If the chip click
    // doesn't also focus this node, the user lands on whichever node
    // was last focused — a different terminal's review.
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    prMock.mockReturnValue(null);
    useUIStore.setState({
      probeOpen: false,
      probeTab: 'files',
    });
    // Pre-focus a DIFFERENT node (not NODE) — if the chip click doesn't
    // also set activeNodeId, the probe would render that other node's
    // review instead of NODE's.
    useAgentNodeStore.setState({ activeNodeId: 999 });
    const { getByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'View changes' }));

    expect(useAgentNodeStore.getState().activeNodeId).toBe(NODE.id);
  });

  it('prefixes the title with "Draft" when the PR is a draft', () => {
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue({
      number: 124,
      url: 'https://github.com/alondero/buildmesh/pull/124',
      title: 'WIP PR chip',
      draft: true,
    });
    const { container } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const chip = container.querySelector('[title^="Draft"]');
    expect(chip).toBeTruthy();
    expect(chip!.getAttribute('title')).toBe('Draft · WIP PR chip');
  });
});

/**
 * Reveal-in-explorer (issue: agent node header â†’ file manager).
 *
 * The header exposes a folder-icon button between `BuildRunDropdown` and
 * maximise that calls `open_in_file_manager` (Tauri command) for the
 * agent's canonical working directory. The IPC does its own WSLâ†’host
 * translation in `src-tauri/src/commands/file_tree.rs`, so the React
 * side just hands it the same `getNodeGitPath(node)` it already uses
 * for git-summary subscriptions.
 *
 * Tests cover three concerns:
 *   1. The button is always rendered (the trio is one control group,
 *      gated by `showInlineActions`, never conditionally hidden).
 *   2. The click resolves the path through `getNodeGitPath` — the
 *      working-tree node resolves to the worktree subdir, the root-mode
 *      node resolves to the mesh root.
 *   3. Errors from the IPC are swallowed (`console.error`, no reject),
 *      matching the precedent in `WorktreeManagerTab.openInExplorer`
 *      and avoiding a toast storm when a worktree row is stale.
 */
describe('GridNodeHeader resume affordance', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useAgentNodeStore.setState({ autopilotStates: {}, circuitOwnerships: {} });
    useMeshStore.setState({
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
    summaryMock.mockReset();
    prMock.mockReset();
    openUrlMock.mockClear();
  });

  it('shows a lost-conversation badge for missing identity outside Autopilot', () => {
    const node = { ...NODE, status: 'suspended' as const, cli_session_id: '' };
    seedAgentNodes([node], node.id);
    useAgentNodeStore.setState({ autopilotStates: {} });
    const { getByText, queryByTestId } = render(<GridNodeHeader nodeId={node.id} onBuildRun={vi.fn()} />);
    expect(getByText('No session')).toBeTruthy();
    expect(queryByTestId('grid-resume-button')).toBeNull();
  });

  it('does not suppress lost-conversation recovery for terminal Circuit history', () => {
    const node = { ...NODE, status: 'suspended' as const, cli_session_id: '' };
    seedAgentNodes([node], node.id);
    useAgentNodeStore.setState({ circuitOwnerships: {
      1: { node_id: 1, run_id: 2, circuit_id: 9, circuit_name: 'Review workflow', state: 'completed', parent_node_id: null },
    } });
    expect(render(<GridNodeHeader nodeId={node.id} onBuildRun={vi.fn()} />).getByText('No session')).toBeTruthy();
  });

  it('renders an inline Resume button when Suspended AND cli_session_id is set', () => {
    const node = {
      ...NODE,
      status: 'suspended' as const,
      cli_session_id: 'a53dd36f-e703-4f27-9356-8e523472d94e',
    };
    seedAgentNodes([node], node.id);
    const { getByTestId } = render(<GridNodeHeader nodeId={node.id} onBuildRun={vi.fn()} />);
    expect(getByTestId('grid-resume-button')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    expect(screen.queryByRole('menuitem', { name: 'Resume agent' })).toBeNull();
    expect(screen.getAllByTestId('grid-resume-button')).toHaveLength(1);
  });

  it('does NOT render a Resume button when Suspended but cli_session_id is null (autopilot gate)', () => {
    // Autopilot-gate Suspended rows are parked at creation with no
    // session id — the autopilot's own "Approve Sandbox Run" action
    // is the recovery surface there. Same gate as the sidebar
    // NodeItem's `showResume` (see its docblock).
    const node = { ...NODE, status: 'suspended' as const, cli_session_id: null };
    seedAgentNodes([node], node.id);
    const { queryByTestId } = render(<GridNodeHeader nodeId={node.id} onBuildRun={vi.fn()} />);
    expect(queryByTestId('grid-resume-button')).toBeNull();
  });

  it('clicking the inline Resume button invokes spawn_agent via the store', async () => {
    // Mirrors the sidebar NodeItem's Resume click test
    // (`node-item-restart-button.test.tsx`) — the inline Resume button
    // re-attempts the same `--resume` the failed auto-resume tried.
    // The store's `spawnAgent` reads `cli_session_id` from the row and
    // passes it as the `resume` arg.
    const node = {
      ...NODE,
      status: 'suspended' as const,
      cli_session_id: 'a53dd36f-e703-4f27-9356-8e523472d94e',
    };
    const spawnAgentMock = vi.fn().mockResolvedValue(undefined);
    useAgentNodeStore.setState({
      nodesById: { [node.id]: node }, nodeIds: [node.id], activeNodeId: node.id,
      spawnAgent: spawnAgentMock,
    });
    const { getByTestId } = render(<GridNodeHeader nodeId={node.id} onBuildRun={vi.fn()} />);
    fireEvent.click(getByTestId('grid-resume-button'));
    await Promise.resolve();
    expect(spawnAgentMock).toHaveBeenCalledWith(node.id, node.provider);
  });

  it('does NOT render a Resume button for non-Suspended statuses regardless of cli_session_id', () => {
    // Sanity: the gate is status-specific. A running / error / idle
    // node with a stored session id must not show the affordance —
    // the existing Restart / Regenerate flows cover those statuses.
    for (const status of ['running', 'idle', 'awaiting_input', 'error', 'archived'] as const) {
      const node = {
        ...NODE,
        status,
        cli_session_id: 'a53dd36f-e703-4f27-9356-8e523472d94e',
      };
      seedAgentNodes([node], node.id);
      const { queryByTestId, unmount } = render(<GridNodeHeader nodeId={node.id} onBuildRun={vi.fn()} />);
      expect(
        queryByTestId('grid-resume-button'),
        `Resume must not render for status=${status}`,
      ).toBeNull();
      unmount();
    }
  });
});

describe('GridNodeHeader observed Muse session telemetry (issue #1680)', () => {
  beforeEach(() => {
    seedAgentNodes([{ ...NODE, provider: 'muse' }], NODE.id);
    useAgentNodeStore.setState({ autopilotStates: {}, circuitOwnerships: {} });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue(null);
    telemetryMock.mockReturnValue({
      kind: 'observed_session_telemetry',
      node_id: NODE.id,
      session_id: 'sess-aaaa-1111',
      model_id: 'muse-spark-1.3-contributor',
      last_turn: {
        turn_id: 'turn-2',
        prompt_tokens: 25,
        output_tokens: 15,
        total_tokens: 40,
        input_tokens: 40,
        reasoning_tokens: 6,
        cached_tokens: 15,
        cache_read_tokens: 15,
        cache_write_tokens: 10,
      },
      cumulative: { prompt_tokens: 45, output_tokens: 25, total_tokens: 70 },
      context: {
        used_tokens: 96000,
        window_tokens: 128000,
        pressure: 0.75,
        pressure_level: 'warning',
      },
    });
  });

  afterEach(() => {
    telemetryMock.mockReturnValue(null);
  });

  it('surfaces observed session telemetry in node details, not as quota', () => {
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: 'Agent node actions' }));
    const panel = screen.getByTestId('observed-session-telemetry');
    expect(panel.textContent).toContain('Observed Session Telemetry');
    expect(panel.textContent).toContain('Session observations — not account quota');
    expect(panel.textContent).toContain('Last turn');
    expect(panel.textContent).toContain('Prompt (counted once) 25');
    expect(panel.textContent).toContain('Session total');
    expect(panel.textContent).toContain('Total 70');
    expect(panel.textContent).toContain('96,000 / 128,000 (75%)');
    expect(panel.textContent).not.toContain('remaining');
    expect(panel.textContent).not.toContain('allowance');
    expect(panel.textContent).not.toContain('$');
  });
});
