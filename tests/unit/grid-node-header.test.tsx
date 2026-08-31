/**
 * The git-summary chip in the Agent Node header used to render all three
 * counts in one muted-grey span, so added / modified / deleted blurred into
 * a single "+3 ~2 -1" smudge that washed out against the mesh tint. Each
 * count now gets its own semantic colour (green / amber / red), zero counts
 * stay muted so the eye lands on the non-zero ones, and an open agent
 * file-explorer panel still overrides the whole chip to cyan to signal
 * which node owns the panel.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, render } from '@testing-library/react';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

// Irrelevant to chip styling — stub it.
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
vi.mock('../../src/hooks/useOpenPr', () => ({
  useOpenPr: () => ({ pr: prMock(), loading: false, refresh: vi.fn() }),
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
const { openInFileManagerMock, toggleNodePinnedMock } = vi.hoisted(() => ({
  openInFileManagerMock: vi.fn().mockResolvedValue(undefined),
  toggleNodePinnedMock: vi.fn(),
}));
vi.mock('../../src/lib/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/tauri')>();
  return {
    ...actual,
    openInFileManager: openInFileManagerMock,
    toggleNodePinned: toggleNodePinnedMock,
  };
});

import { GridNodeHeader } from '../../src/components/AgentNodeView/GridNodeHeader';
import { AUTOPILOT_PILL_STYLES } from '../../src/components/AgentNodeView/GridNodeHeader';
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

describe('GridNodeHeader git-summary chip', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    summaryMock.mockReset();
    prMock.mockReset();
    openUrlMock.mockClear();
  });

  it('colours added / modified / deleted counts distinctly so the diff pops', () => {
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    const { getByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-accent-green');
    expect(getByText('~2').className).toContain('text-accent-amber');
    expect(getByText('-1').className).toContain('text-accent-red');
  });

  it('mutes zero counts so the eye is drawn to the non-zero changes', () => {
    summaryMock.mockReturnValue({ total: 3, added: 3, modified: 0, deleted: 0 });
    const { getByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-accent-green');
    expect(getByText('~0').className).toContain('text-text-muted');
    expect(getByText('-0').className).toContain('text-text-muted');
  });

  it('overrides per-status colour with cyan when the probe is showing this node\'s review (#376)', () => {
    // Issue #376 — the cyan "this is the node being reviewed" highlight
    // follows the Probe Panel state (probeOpen + probeTab === 'review'
    // + activeNodeId === node.id).
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    prMock.mockReturnValue(null);
    useUIStore.setState({
      probeOpen: true,
      probeTab: 'review',
    });
    useAgentNodeStore.setState({ activeNodeId: NODE.id });
    const { getByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-accent-cyan');
    expect(getByText('~2').className).toContain('text-accent-cyan');
    expect(getByText('-1').className).toContain('text-accent-cyan');
  });

  it('keeps the per-status colours when the probe is on review but a different node is focused', () => {
    // The cyan override is per-node: it should only fire for the node
    // whose review the probe is actually showing.
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    prMock.mockReturnValue(null);
    useUIStore.setState({
      probeOpen: true,
      probeTab: 'review',
    });
    useAgentNodeStore.setState({ activeNodeId: 999 /* not this node */ });
    const { getByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-accent-green');
    expect(getByText('~2').className).toContain('text-accent-amber');
    expect(getByText('-1').className).toContain('text-accent-red');
  });
});

describe('GridNodeHeader worktree/root pill', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    summaryMock.mockReset();
  });

  it('shows a muted "worktree" pill when the node runs in a worktree', () => {
    summaryMock.mockReturnValue(null);
    const overrideNode = { ...NODE, use_worktree: true };
    seedAgentNodes([overrideNode]);
    const { getByText } = render(
      <GridNodeHeader nodeId={overrideNode.id} onBuildRun={() => {}} />
    );
    const pill = getByText('worktree');
    expect(pill.className).toContain('text-text-muted');
    expect(pill.className).not.toContain('text-accent-cyan');
  });

  it('shows a cyan "root" pill when the node runs in the repository root', () => {
    summaryMock.mockReturnValue(null);
    const { getByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const pill = getByText('root');
    expect(pill.className).toContain('text-accent-cyan');
    expect(pill.className).toContain('font-semibold');
  });

  it('gives the pill a tooltip explaining what the label means', () => {
    summaryMock.mockReturnValue(null);
    const worktreeNode = { ...NODE, use_worktree: true };
    useAgentNodeStore.setState({ nodesById: { [worktreeNode.id]: worktreeNode }, nodeIds: [worktreeNode.id] });
    const { container, rerender } = render(
      <GridNodeHeader nodeId={worktreeNode.id} onBuildRun={() => {}} />
    );
    expect(container.querySelector('[title="Agent runs in a git worktree"]')).toBeTruthy();

    // Restore the non-worktree state so the rerender reads the root path.
    useAgentNodeStore.setState({
      nodesById: { [NODE.id]: NODE },
      nodeIds: [NODE.id],
    });
    rerender(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(container.querySelector('[title="Agent runs in the repository root"]')).toBeTruthy();
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

describe('GridNodeHeader pin toggle (wayfinder #982 / #985)', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue(null);
    toggleNodePinnedMock.mockReset();
  });

  it('renders an unpressed "Pin node" toggle when the node is unpinned', () => {
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const button = getByLabelText('Pin node');
    expect(button.getAttribute('aria-pressed')).toBe('false');
    expect(button.getAttribute('title')).toBe('Pin node');
  });

  it('renders a pressed "Unpin node" toggle when the node is pinned', () => {
    const pinned = { ...NODE, is_pinned: true };
    useAgentNodeStore.setState({ nodesById: { [pinned.id]: pinned }, nodeIds: [pinned.id] });
    seedAgentNodes([pinned]);
    const { getByLabelText } = render(<GridNodeHeader nodeId={pinned.id} onBuildRun={() => {}} />);
    const button = getByLabelText('Unpin node');
    expect(button.getAttribute('aria-pressed')).toBe('true');
    expect(button.getAttribute('title')).toBe('Unpin node');
  });

  it('sits immediately before the maximize button in DOM order (#985)', () => {
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const all = Array.from(document.querySelectorAll('button'));
    const pinIndex = all.indexOf(getByLabelText('Pin node'));
    const maxIndex = all.indexOf(getByLabelText('Maximize agent node'));
    expect(pinIndex).toBeGreaterThanOrEqual(0);
    expect(maxIndex).toBe(pinIndex + 1);
  });

  it('invokes the shared store action, which calls toggle_node_pinned and patches the store', async () => {
    toggleNodePinnedMock.mockResolvedValueOnce({ ...NODE, is_pinned: true });
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireEvent.click(getByLabelText('Pin node'));
    expect(toggleNodePinnedMock).toHaveBeenCalledWith(NODE.id);
    // The store's optimistic patch lands without waiting for the IPC.
    const { waitFor } = await import('@testing-library/react');
    await waitFor(() => {
      // Issue #1384 — read by id from the normalized map.
      expect(useAgentNodeStore.getState().nodesById[NODE.id]?.is_pinned).toBe(true);
    });
  });

  it('shares the inline action surface (bg-bg-base/60 + border, 7Ã—7) with the maximise/close trio', () => {
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const cls = getByLabelText('Pin node').className;
    expect(cls).toMatch(/\bh-7\b/);
    expect(cls).toMatch(/\bw-7\b/);
    expect(cls).toContain('bg-bg-base/60');
    expect(cls).toContain('border-border-default');
  });
});

/**
 * The close + maximize buttons used to render with `opacity-0
 * group-hover:opacity-100` and `text-text-muted`, so against the per-mesh
 * tinted header background they were effectively invisible until the user
 * hovered over the row. Each control now carries a `bg-bg-base/60` surface
 * plus a 1px border (matching BuildRunDropdown's trigger) so they read at
 * rest, and use `text-text-primary` so the icon shape is legible against
 * the mesh tint instead of fading into it.
 */
describe('GridNodeHeader close + expand controls (icon visibility)', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue(null);
  });

  it('the maximize button is visible at rest (no opacity-0) and has a button surface', () => {
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const button = getByLabelText('Maximize agent node');
    const cls = button.className;
    // Regression guard for the "invisible until hover" bug: the control
    // must NOT be hidden at rest.
    expect(cls).not.toMatch(/\bopacity-0\b/);
    expect(cls).not.toMatch(/\bgroup-hover:opacity-100\b/);
    // Surface treatment — the dark fill pops against the mesh-tinted header.
    expect(cls).toContain('bg-bg-base/60');
    expect(cls).toContain('border');
    expect(cls).toContain('border-border-default');
    // Use a fully-visible text colour so the icon shape reads against the
    // mesh tint, not a near-invisible grey.
    expect(cls).toContain('text-text-primary');
    expect(cls).not.toContain('text-text-muted');
  });

  it('the close button is visible at rest (no opacity-0) and has a button surface', () => {
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const button = getByLabelText('Close agent node');
    const cls = button.className;
    expect(cls).not.toMatch(/\bopacity-0\b/);
    expect(cls).not.toMatch(/\bgroup-hover:opacity-100\b/);
    expect(cls).toContain('bg-bg-base/60');
    expect(cls).toContain('border');
    expect(cls).toContain('border-border-default');
    expect(cls).toContain('text-text-primary');
    expect(cls).not.toContain('text-text-muted');
  });

  it('keeps the cyan hover treatment on the maximize button so the intent still reads', () => {
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const cls = getByLabelText('Maximize agent node').className;
    // Hover must still flip to the cyan accent — that's what signals
    // "this is the maximise command" at a glance.
    expect(cls).toContain('hover:text-accent-cyan');
    expect(cls).toContain('hover:bg-accent-cyan');
  });

  it('keeps the red hover treatment on the close button so the destructive intent reads', () => {
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const cls = getByLabelText('Close agent node').className;
    expect(cls).toContain('hover:text-status-error');
    expect(cls).toContain('hover:bg-status-error-bg');
  });

  it('maximise + close share the same h-7 w-7 surface as BuildRunDropdown for visual balance', () => {
    // Pinned here so a future tweak to GridNodeHeader keeps the maximise
    // and close surface matching BuildRunDropdown's. The Build-side
    // counterpart lives in build-run-dropdown.test.tsx — that file mocks
    // nothing about BuildRunDropdown, so the assertion can hit the real DOM.
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    for (const label of ['Maximize agent node', 'Close agent node']) {
      const cls = getByLabelText(label).className;
      expect(cls).toMatch(/\bh-7\b/);
      expect(cls).toMatch(/\bw-7\b/);
      expect(cls).toContain('bg-bg-base/60');
      expect(cls).toContain('border-border-default');
    }
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

  it('opens the PR in the default browser when clicked', () => {
    summaryMock.mockReturnValue(null);
    const url = 'https://github.com/alondero/buildmesh/pull/123';
    prMock.mockReturnValue({ number: 123, url, title: 'Add PR chip', draft: false });
    const { getByText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    fireEvent.click(getByText('PR #123'));
    expect(openUrlMock).toHaveBeenCalledWith(url);
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

    fireEvent.click(getByText('+3'));

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

    fireEvent.click(getByText('+3'));

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
describe('GridNodeHeader reveal-in-explorer action', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue(null);
    openInFileManagerMock.mockReset();
    openInFileManagerMock.mockResolvedValue(undefined);
  });

  it('renders a folder-icon button with the expected aria-label', () => {
    // Same label as `<PathHeader>` — one verb, one announcement across
    // the app (review caught this when both used different strings).
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(getByLabelText('Open in file explorer')).toBeTruthy();
  });

  it('shares the inline trio surface treatment (bg-bg-base/60 + border, 7Ã—7)', () => {
    // Mirrors the surface pinning in `close + expand controls` describe
    // above, so a future tweak keeps the trio reading as one control
    // group rather than a tacked-on folder icon.
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const cls = getByLabelText('Open in file explorer').className;
    expect(cls).toMatch(/\bw-7\b/);
    expect(cls).toMatch(/\bh-7\b/);
    expect(cls).toContain('bg-bg-base/60');
    expect(cls).toContain('border-border-default');
    expect(cls).toContain('text-text-primary');
    // Visibility regression guard — the same opacity-0 bug that hit
    // maximise/close must not reappear here.
    expect(cls).not.toMatch(/\bopacity-0\b/);
    expect(cls).not.toMatch(/\bgroup-hover:opacity-100\b/);
  });

  it('uses an accent-cyan hover matching the existing PathHeader precedent', () => {
    // Hover treatment deliberately reuses `<PathHeader>`'s `accent-cyan`
    // so the verb reads the same wherever the user encounters it
    // (review caught a divergent blue here previously).
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const cls = getByLabelText('Open in file explorer').className;
    expect(cls).toContain('hover:text-accent-cyan');
    // And it MUST NOT collide with the close button's error semantic.
    expect(cls).not.toContain('status-error');
  });

  it('sits to the left of the maximise button in DOM order', () => {
    // The placement was a user request: "between the build and maximise
    // icons". (BuildRunDropdown is stubbed to render null in this file,
    // so we anchor layout purely on the inline trio.)
    const { getByLabelText, container } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const all = Array.from(container.querySelectorAll('button'));
    const revealIndex = all.indexOf(getByLabelText('Open in file explorer'));
    const maxIndex = all.indexOf(getByLabelText('Maximize agent node'));
    expect(revealIndex).toBeLessThan(maxIndex);
  });

  it('passes the mesh root to openInFileManager when the node runs in root mode', () => {
    // `use_worktree: false` â‡’ `getNodeGitPath` returns the mesh root
    // verbatim. Verify the click handler resolves the same path the
    // git-summary chip subscribes to — otherwise the user opens a
    // different folder than the one git status is reporting on.
    const node: AgentNode = { ...NODE, use_worktree: false };
    useAgentNodeStore.setState({ nodesById: { [node.id]: node }, nodeIds: [node.id] });
    const { getByLabelText } = render(<GridNodeHeader nodeId={node.id} onBuildRun={() => {}} />);
    fireEvent.click(getByLabelText('Open in file explorer'));
    expect(openInFileManagerMock).toHaveBeenCalledWith('/repo');
  });

  it('passes the worktree subdir when the node runs in a worktree', () => {
    // `use_worktree: true` â‡’ `getNodeGitPath` returns
    // `<mesh>/.claude/worktrees/<name>`. The user expects to be dropped
    // into the same directory the agent is editing, not the parent.
    const node: AgentNode = { ...NODE, use_worktree: true, worktree_name: 'feature-x' };
    useAgentNodeStore.setState({ nodesById: { [node.id]: node }, nodeIds: [node.id] });
    const { getByLabelText } = render(<GridNodeHeader nodeId={node.id} onBuildRun={() => {}} />);
    fireEvent.click(getByLabelText('Open in file explorer'));
    expect(openInFileManagerMock).toHaveBeenCalledWith('/repo/.claude/worktrees/feature-x');
  });

  it('falls back to the mesh root when a worktree-mode node has no worktree_name', () => {
    // Defensive: `getNodeGitPath` falls back when worktree_name is null
    // (the row can be published before the worktree is provisioned);
    // pin the fallback here so a future "throw if no worktree" change
    // in the path helper doesn't silently break reveal.
    const node: AgentNode = { ...NODE, use_worktree: true, worktree_name: null };
    useAgentNodeStore.setState({ nodesById: { [node.id]: node }, nodeIds: [node.id] });
    const { getByLabelText } = render(<GridNodeHeader nodeId={node.id} onBuildRun={() => {}} />);
    fireEvent.click(getByLabelText('Open in file explorer'));
    expect(openInFileManagerMock).toHaveBeenCalledWith('/repo');
  });

  it('tool-tip surfaces the resolved path so users know which folder will open', () => {
    // Worktree nodes in particular open into a non-obvious subdir; the
    // tooltip is the only affordance that previews the destination
    // before the click.
    const node: AgentNode = { ...NODE, use_worktree: true, worktree_name: 'feature-x' };
    useAgentNodeStore.setState({ nodesById: { [node.id]: node }, nodeIds: [node.id] });
    const { getByLabelText } = render(<GridNodeHeader nodeId={node.id} onBuildRun={() => {}} />);
    const btn = getByLabelText('Open in file explorer');
    expect(btn.getAttribute('title')).toBe(
      'Open in file explorer (/repo/.claude/worktrees/feature-x)',
    );
  });

  it('swallows IPC errors with console.error (no unhandled rejection)', async () => {
    // The Tauri command rejects with `"Path does not exist"` when the
    // worktree has been pruned between renders — exactly the case
    // WorktreeManagerTab.openInExplorer handles by silencing. If we
    // let it bubble here, every stale-row click would land an unhandled
    // rejection in the console. Verify the regression guard.
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    openInFileManagerMock.mockRejectedValueOnce('Path does not exist: /old/worktree');
    const { getByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);

    // Click is sync; the handler catches and console.errors. If the
    // try/catch were ever removed, the rejected promise would surface
    // as an unhandled rejection — Vitest reports those as failures.
    fireEvent.click(getByLabelText('Open in file explorer'));
    // Drain microtasks so the rejection observer would have fired.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(errSpy).toHaveBeenCalledWith(
      'Failed to open folder in file manager:',
      expect.stringContaining('Path does not exist'),
    );
    errSpy.mockRestore();
  });
});

/**
 * Autopilot pill — surfaces which nodes automation owns and where each is
 * in the pipeline, driven by `autopilotStates` in the agent-node store
 * (hydrated from `list_autopilot_runs` + patched by `autopilot-*` events).
 */
describe('GridNodeHeader autopilot pill', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ nodesById: { [NODE.id]: NODE }, nodeIds: [NODE.id], activeNodeId: NODE.id, autopilotStates: {} });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue(null);
  });

  it('is absent for a hand-spawned node (no autopilot run)', () => {
    const { queryByTestId } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(queryByTestId('autopilot-pill')).toBeNull();
  });

  it('shows a violet "autopilot" pill while the agent is implementing', () => {
    useAgentNodeStore.setState({ autopilotStates: { [NODE.id]: 'implementing' } });
    const { getByTestId } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const pill = getByTestId('autopilot-pill');
    expect(pill.textContent).toBe('autopilot');
    expect(pill.className).toContain('text-accent-violet');
  });

  it('flips to the amber wrap-up treatment while finishing', () => {
    useAgentNodeStore.setState({ autopilotStates: { [NODE.id]: 'finishing' } });
    const { getByTestId } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const pill = getByTestId('autopilot-pill');
    expect(pill.textContent).toContain('wrap-up');
    expect(pill.className).toContain('text-accent-amber');
  });

  // Issue #993 — a loop iteration's deterministic wrap-up has passed and the
  // optional `loop_suffix_prompt` is driving a second PTY turn on the same
  // node. Renders with the same amber family as `finishing` but its own
  // label, so a user can tell that the iteration is still in flight while a
  // final prompt runs (rather than waiting for wrap-up to be re-verified).
  it('flips to the amber suffix treatment while the suffix turn is in flight', () => {
    useAgentNodeStore.setState({ autopilotStates: { [NODE.id]: 'suffix_pending' } });
    const { getByTestId } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const pill = getByTestId('autopilot-pill');
    expect(pill.textContent).toContain('suffix');
    expect(pill.className).toContain('text-accent-amber');
  });

  it('shows green when completed and red when failed', () => {
    useAgentNodeStore.setState({ autopilotStates: { [NODE.id]: 'completed' } });
    const { getByTestId, rerender } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(getByTestId('autopilot-pill').className).toContain('text-accent-green');
    // The label must say it, not just tint it: a bare checkmark reads as
    // decoration, and "autopilot done, hands off, awaiting your PR review"
    // is the state the user acts on.
    expect(getByTestId('autopilot-pill').textContent).toContain('complete');

    useAgentNodeStore.setState({ autopilotStates: { [NODE.id]: 'failed' } });
    rerender(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(getByTestId('autopilot-pill').className).toContain('text-status-error');
  });

  it('the typed union means a backend adding a new state would surface here as a TS error', () => {
    // Regression guard: the pill style map is `Record<AutopilotRunState, …>`
    // (the typed union generated from the Rust enum via ts-rs), so a
    // future backend state that isn't in the map would fail to compile
    // rather than silently degrading to a default. The pin: the map and
    // the union live in lockstep on both sides of the wire.
    expect(Object.keys(AUTOPILOT_PILL_STYLES).sort()).toEqual(
      ['completed', 'failed', 'finishing', 'implementing', 'merged', 'suffix_pending'],
    );
  });

  it('only badges the node the run belongs to', () => {
    useAgentNodeStore.setState({ autopilotStates: { 999: 'implementing' } });
    const { queryByTestId } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(queryByTestId('autopilot-pill')).toBeNull();
  });
});

// Manual `/finish` trigger (issue #484, PRD #480 story 15). Behavioural
// assertion (the invoke effect, not `={noop}` wiring — the JSX
// silent-extra-prop lesson): clicking the button hands the node id to the
// `trigger_finish` IPC wrapper.
/**
 * Issue #484 originally shipped a manual `/finish` trigger on the agent node
 * header. The wrap-up sequence is purely an autopilot concern — there is no
 * value in letting a human re-trigger it on a hand-controlled node — so the
 * toolbar icon (inline + kebab menu) and the underlying IPC were retired.
 *
 * These assertions pin the removal: no button, no menu item, no leftover
 * `trigger_finish` wiring on the surface. The autopilot's automatic
 * wrap-up injection (`pipeline.rs`) is a separate code path that still emits
 * the `autopilot-finishing` event for the pill transition.
 *
 * Width detail: `useResizeWidth` initialises at `Infinity`, so the first
 * render lands in the `xl` tier with `showInlineActions === true` and the
 * inline trio (reveal-in-explorer + pin + maximise + close) renders inline.
 * That same code path USED TO render the Finish button here — its absence
 * after this commit is what we're asserting.
 */
describe('GridNodeHeader Finish trigger (removed — autopilot-only concern)', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue(null);
  });

  it('does not render an inline Finish button at the wide (xl) tier', () => {
    // xl tier: `useResizeWidth` boots at Infinity, so before the
    // ResizeObserver fires the inline branch is active. The Finish
    // button WOULD render here if the wiring were still in place.
    const { queryByLabelText } = render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    expect(queryByLabelText('Finish agent node')).toBeNull();
    expect(queryByLabelText(/Finish/i)).toBeNull();
  });

  // Note: kebab-without-Finish coverage lives in
  // `grid-node-header-responsive.test.tsx` — that test opens the real
  // menu via `fireResize(<header>, 200)` and asserts `toHaveLength(4)`
  // with `items[3]` = "Close agent node". A second assertion here that
  // never opens the kebab would only re-prove the inline path.
});

// ---------------------------------------------------------------------------
// User-driven Resume affordance — mirrors the sidebar NodeItem's `showResume`.
// The same `cli_session_id` gate keeps autopilot-gate Suspended rows (no
// session id) from surfacing a confusing "no CLI session ID is stored"
// toast on click. The kebab's fifth item is covered separately by the
// responsive-tier test (which resizes to `slim` and asserts the menu row).
// ---------------------------------------------------------------------------

describe('GridNodeHeader resume affordance', () => {
  beforeEach(() => {
    seedAgentNodes([NODE], NODE.id);
    useMeshStore.setState({
      meshesById: new Map([[MESH.id, MESH]]),
      selectedMeshId: MESH.id,
    });
    summaryMock.mockReset();
    prMock.mockReset();
    openUrlMock.mockClear();
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
