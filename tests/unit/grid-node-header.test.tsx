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

import { GridNodeHeader } from '../../src/components/SessionView/GridNodeHeader';

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
  created_at: new Date(0).toISOString(),
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
    useUIStore.setState({ fileExplorerContext: null });
    summaryMock.mockReset();
    prMock.mockReset();
    openUrlMock.mockClear();
  });

  it('colours added / modified / deleted counts distinctly so the diff pops', () => {
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-green-400');
    expect(getByText('~2').className).toContain('text-amber-400');
    expect(getByText('-1').className).toContain('text-red-400');
  });

  it('mutes zero counts so the eye is drawn to the non-zero changes', () => {
    summaryMock.mockReturnValue({ total: 3, added: 3, modified: 0, deleted: 0 });
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-green-400');
    expect(getByText('~0').className).toContain('text-text-muted');
    expect(getByText('-0').className).toContain('text-text-muted');
  });

  it('overrides per-status colour with cyan when this node owns the agent file-explorer panel', () => {
    summaryMock.mockReturnValue({ total: 6, added: 3, modified: 2, deleted: 1 });
    useUIStore.setState({
      fileExplorerContext: { type: 'agent', nodeId: NODE.id, path: '/repo' },
    });
    const { getByText } = render(<GridNodeHeader node={NODE} onBuildRun={() => {}} />);

    expect(getByText('+3').className).toContain('text-accent-cyan');
    expect(getByText('~2').className).toContain('text-accent-cyan');
    expect(getByText('-1').className).toContain('text-accent-cyan');
  });
});

describe('GridNodeHeader worktree/root pill', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ fileExplorerContext: null });
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

describe('GridNodeHeader PR chip', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useUIStore.setState({ fileExplorerContext: null });
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
    expect(chip.className).toContain('text-green-400');
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
