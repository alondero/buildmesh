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

import { GridNodeHeader } from '../../src/components/AgentNodeView/GridNodeHeader';

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
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    summaryMock.mockReset();
    prMock.mockReset();
    openUrlMock.mockClear();
  });

  it('colours added / modified / deleted counts distinctly so the diff pops', () => {
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-accent-green');
    expect(getByText('~2').className).toContain('text-accent-amber');
    expect(getByText('-1').className).toContain('text-accent-red');
  });

  it('mutes zero counts so the eye is drawn to the non-zero changes', () => {
    summaryMock.mockReturnValue({ total: 3, added: 3, modified: 0, deleted: 0 });
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

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
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

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
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-accent-green');
    expect(getByText('~2').className).toContain('text-accent-amber');
    expect(getByText('-1').className).toContain('text-accent-red');
  });
});

describe('GridNodeHeader worktree/root pill', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    summaryMock.mockReset();
  });

  it('shows a muted "worktree" pill when the node runs in a worktree', () => {
    summaryMock.mockReturnValue(null);
    const { getByText } = render(
      <GridNodeHeader node={{ ...NODE, use_worktree: true }} onBuildRun={() => {}} />
    );
    const pill = getByText('worktree');
    expect(pill.className).toContain('text-text-muted');
    expect(pill.className).not.toContain('text-accent-cyan');
  });

  it('shows a cyan "root" pill when the node runs in the repository root', () => {
    summaryMock.mockReturnValue(null);
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    const pill = getByText('root');
    expect(pill.className).toContain('text-accent-cyan');
    expect(pill.className).toContain('font-semibold');
  });

  it('gives the pill a tooltip explaining what the label means', () => {
    summaryMock.mockReturnValue(null);
    const { container, rerender } = render(
      <GridNodeHeader node={{ ...NODE, use_worktree: true }} onBuildRun={() => {}} />
    );
    expect(container.querySelector('[title="Agent runs in a git worktree"]')).toBeTruthy();

    rerender(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    expect(container.querySelector('[title="Agent runs in the repository root"]')).toBeTruthy();
  });
});

describe('GridNodeHeader maximize (#65)', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ maximizedNodeId: null });
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue(null);
  });

  it('double-clicking the header maximizes this node', () => {
    const { container } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    // The header root carries the double-click handler.
    fireEvent.doubleClick(container.firstChild as Element);
    expect(useUIStore.getState().maximizedNodeId).toBe(NODE.id);
  });

  it('double-clicking again restores the grid', () => {
    const { container } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    fireEvent.doubleClick(container.firstChild as Element);
    fireEvent.doubleClick(container.firstChild as Element);
    expect(useUIStore.getState().maximizedNodeId).toBe(null);
  });

  it('the explicit button toggles maximize and flips its label', () => {
    const { getByLabelText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    fireEvent.click(getByLabelText('Maximize agent node'));
    expect(useUIStore.getState().maximizedNodeId).toBe(NODE.id);
    // Once maximized, the same control offers "restore".
    fireEvent.click(getByLabelText('Restore grid layout'));
    expect(useUIStore.getState().maximizedNodeId).toBe(null);
  });

  // Issue #668 — Alt+G (Win/Linux) / Cmd+G (macOS) is the keyboard
  // counterpart to the double-click and the explicit button. The header
  // tooltip must surface it so users discover the shortcut without
  // hunting through the empty-state splash.
  it('mentions the platform-specific Alt+G / ⌘+G shortcut in the maximize tooltip', () => {
    const { container } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    const title = (container.firstChild as Element).getAttribute('title') ?? '';
    // Canonical sentence — substring-only asserts would accept broken
    // strings like "Alt+G to do something unrelated maximize".
    const isMac = navigator.platform.toUpperCase().includes('MAC');
    const expected = isMac
      ? 'Double-click or press ⌘+G to maximize'
      : 'Double-click or press Alt+G to maximize';
    expect(title).toBe(expected);
  });

  it('mentions the shortcut in the restore tooltip when the node is maximized', () => {
    useUIStore.setState({ maximizedNodeId: NODE.id });
    const { container } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    const title = (container.firstChild as Element).getAttribute('title') ?? '';
    const isMac = navigator.platform.toUpperCase().includes('MAC');
    const expected = isMac
      ? 'Double-click or press ⌘+G to restore grid'
      : 'Double-click or press Alt+G to restore grid';
    expect(title).toBe(expected);
  });

  // Issue #668 — the explicit maximize button (visible on hover) must
  // advertise the same shortcut so discoverability isn't gated on the
  // header-bar double-click affordance.
  it('the maximize button tooltip mentions the Alt+G / ⌘+G shortcut', () => {
    const { getByLabelText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    const button = getByLabelText('Maximize agent node');
    const title = button.getAttribute('title') ?? '';
    const isMac = navigator.platform.toUpperCase().includes('MAC');
    expect(title).toMatch(isMac ? /⌘\+G/ : /Alt\+G/);
    expect(title.toLowerCase()).toContain('maximize');
  });

  it('the restore button tooltip mentions the shortcut when maximized', () => {
    useUIStore.setState({ maximizedNodeId: NODE.id });
    const { getByLabelText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    const button = getByLabelText('Restore grid layout');
    const title = button.getAttribute('title') ?? '';
    const isMac = navigator.platform.toUpperCase().includes('MAC');
    expect(title).toMatch(isMac ? /⌘\+G/ : /Alt\+G/);
    expect(title.toLowerCase()).toContain('restore');
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
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ maximizedNodeId: null });
    summaryMock.mockReturnValue(null);
    prMock.mockReturnValue(null);
  });

  it('the maximize button is visible at rest (no opacity-0) and has a button surface', () => {
    const { getByLabelText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
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
    const { getByLabelText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
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
    const { getByLabelText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    const cls = getByLabelText('Maximize agent node').className;
    // Hover must still flip to the cyan accent — that's what signals
    // "this is the maximise command" at a glance.
    expect(cls).toContain('hover:text-accent-cyan');
    expect(cls).toContain('hover:bg-accent-cyan');
  });

  it('keeps the red hover treatment on the close button so the destructive intent reads', () => {
    const { getByLabelText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    const cls = getByLabelText('Close agent node').className;
    expect(cls).toContain('hover:text-status-error');
    expect(cls).toContain('hover:bg-status-error-bg');
  });

  it('maximise + close share the same h-7 w-7 surface as BuildRunDropdown for visual balance', () => {
    // Pinned here so a future tweak to GridNodeHeader keeps the maximise
    // and close surface matching BuildRunDropdown's. The Build-side
    // counterpart lives in build-run-dropdown.test.tsx — that file mocks
    // nothing about BuildRunDropdown, so the assertion can hit the real DOM.
    const { getByLabelText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
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
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
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
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
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
    const { queryByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    expect(queryByText(/^PR #/)).toBeNull();
  });

  it('opens the PR in the default browser when clicked', () => {
    summaryMock.mockReturnValue(null);
    const url = 'https://github.com/alondero/buildmesh/pull/123';
    prMock.mockReturnValue({ number: 123, url, title: 'Add PR chip', draft: false });
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

    fireEvent.click(getByText('PR #123'));
    expect(openUrlMock).toHaveBeenCalledWith(url);
  });

  it('opens the probe panel on the review tab when the git-summary chip is clicked (#376)', () => {
    // Issue #376 — clicking the +/~/- chip opens the unified Probe Panel
    // on the 🔍 tab so the user lands on the active agent's
    // AgentReviewPanel.
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    prMock.mockReturnValue(null);
    useUIStore.setState({
      probeOpen: false,
      probeTab: 'files',
    });
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

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
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

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
    const { container } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);
    const chip = container.querySelector('[title^="Draft"]');
    expect(chip).toBeTruthy();
    expect(chip!.getAttribute('title')).toBe('Draft · WIP PR chip');
  });
});
