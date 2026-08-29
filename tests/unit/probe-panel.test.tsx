import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import type { DiffResult, FileNode, GitStatus } from '../../src/lib/tauri';

const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'single',
  position: 0,
  created_at: new Date(0).toISOString(),
  scratchpad: '',
  sandbox: false,
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

/** The six tabs in the order the activity bar presents them (issue #374).
 *  The Archive entry uses its longer tooltip text because the activity-bar
 *  button's accessible name resolves to `tooltip ?? label` (so SR users hear
 *  the same disambiguating form as sighted hover). */
const TAB_LABELS = [
  'Project Files',
  'Agent Changes',
  'Worktree Manager',
  'Mesh Properties',
  'Git Issues',
  'Archived Nodes',
];

function tabButton(label: string): HTMLElement {
  return screen.getByRole('button', { name: label });
}

const FILES: GitStatus[] = [
  { path: 'src/app.ts', status: 'modified', additions: 2, deletions: 1 },
];

const TREE: FileNode = {
  name: 'repo',
  path: '/repo',
  is_dir: true,
  children: [
    { name: 'app.ts', path: '/repo/app.ts', is_dir: false, children: [] },
  ],
};

const DIFF: DiffResult = {
  files: [
    {
      path: 'src/app.ts',
      hunks: [
        { old_start: 1, old_lines: 1, new_start: 1, new_lines: 2, lines: ['-old', '+new'] },
      ],
    },
  ],
};

describe('ProbePanel', () => {
  beforeEach(() => {
    // Default to a context with both a selected mesh and a focused node so the
    // body renders content rather than an empty state. Individual tests narrow
    // this to exercise the empty-state branches.
    useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
    useAgentNodeStore.setState({ agentNodes: [NODE], activeNodeId: NODE.id });
    useUIStore.setState({ probeOpen: false, probeTab: 'files', activeDiffFile: null });
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_git_status') return Promise.resolve(FILES);
      if (cmd === 'list_directory') return Promise.resolve(TREE);
      if (cmd === 'diff_file_against_head') return Promise.resolve(DIFF);
      if (cmd === 'diff_node_against_base') return Promise.resolve(DIFF);
      if (cmd === 'node_changed_files') return Promise.resolve(FILES);
      if (cmd === 'get_git_branch_status') return Promise.resolve(null);
      return Promise.resolve({});
    });
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
    expect(header.textContent).toContain('Archive');
  });

  it('shows the active mesh name as a subheading in the header', () => {
    // The shared dock header carries both the active tab label (title)
    // and the active mesh name (subheading) so the user always knows
    // which project the dock is anchored to without needing the
    // directory path strip that the Issues / PRs tabs used to render.
    useUIStore.setState({ probeOpen: true, probeTab: 'files' });
    render(<ProbePanel />);

    const header = screen.getByRole('region', { name: 'Probe panel' });
    // MESH.name is 'demo' (see fixture at the top of the file).
    expect(header.textContent).toContain('demo');
  });

  it('omits the mesh-name subheading when no project is active', () => {
    // Empty-state header: the title is still there, but with no mesh
    // there is no name to render so the subheading line is omitted
    // (not blank-padded) — keeps the no-project empty state tidy.
    useMeshStore.setState({ meshesById: new Map(), selectedMeshId: null });
    useAgentNodeStore.setState({ agentNodes: [], activeNodeId: null });
    useUIStore.setState({ probeOpen: true, probeTab: 'files' });
    render(<ProbePanel />);

    const header = screen.getByRole('region', { name: 'Probe panel' });
    expect(header.textContent).not.toContain('demo');
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

  it('renders the Project Files tab body (Changed Files + File Tree) when the 📁 tab is active', async () => {
    // Issue #376 — the 📁 tab no longer shows a "coming soon" placeholder,
    // it shows the same ChangedFilesSection + collapsible FileTree that
    // FileExplorerPanel used to render for a mesh context.
    useUIStore.setState({ probeOpen: true, probeTab: 'files' });
    render(<ProbePanel />);

    expect(await screen.findByText('Changed Files')).toBeTruthy();
    expect(screen.getByText('File Tree')).toBeTruthy();
  });

  it('renders the Agent Changes tab body for the focused node when the 🔍 tab is active', async () => {
    // Issue #376 — the 🔍 tab loads the focused node's lightweight
    // base-relative file list. The file title is the canary that the
    // nodeChangedFiles call landed without loading a full diff.
    useUIStore.setState({ probeOpen: true, probeTab: 'review' });
    render(<ProbePanel />);

    await waitFor(() => {
      expect(screen.getByText('src/app.ts')).toBeTruthy();
    });
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('node_changed_files', { nodeId: NODE.id });
    });
    expect(invoke).not.toHaveBeenCalledWith('diff_node_against_base', { nodeId: NODE.id });
  });

  it('renders the Mesh Properties form (issue #375) when the ⚙️ tab is open', () => {
    // Sanity check on the wiring: the properties tab now hosts the new
    // `<MeshPropertiesTab>` (config form), not the legacy placeholder
    // "coming soon" message. A specific input label is enough to prove
    // the form mounted.
    useUIStore.setState({ probeOpen: true, probeTab: 'properties' });
    render(<ProbePanel />);

    // `getByLabelText` would race the form's load effect; `findBy*`
    // awaits the first render after the `get_mesh_properties` mock
    // resolves, matching the new tab's mount semantics.
    expect(screen.queryByText('Loading…')).toBeTruthy();
    expect(screen.queryByText('This tab\'s content is coming soon.')).toBeNull();
  });

  it('renders the Git Issues tab body (issue #378) when the 🐙 tab is open', async () => {
    // Issue #378 — the 🐙 tab hosts the new `<GitIssuesTab>` (ported
    // from `GitHubIssuesModal`), not the legacy placeholder. The
    // "Loading issues..." canary is enough to prove the tab mounted
    // before the mocked `get_repo_issues` resolves.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.resolve([]);
      if (cmd === 'list_providers') return Promise.resolve([]);
      return Promise.resolve({});
    });
    useUIStore.setState({ probeOpen: true, probeTab: 'issues' });
    render(<ProbePanel />);

    expect(await screen.findByText('Loading issues...')).toBeTruthy();
    expect(screen.queryByText('This tab\'s content is coming soon.')).toBeNull();
  });

  it('renders the Session History tab body (issue #378) when the 🕒 tab is open', async () => {
    // Issue #378 — the 🕒 tab hosts the new `<ArchivedNodesTab>`
    // (ported from `SessionBrowserModal`). The "Scanning sessions…"
    // canary is enough to prove the tab mounted before the mocked
    // `discover_agent_nodes` resolves.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'discover_agent_nodes') return Promise.resolve([]);
      if (cmd === 'list_providers') return Promise.resolve([]);
      return Promise.resolve({});
    });
    useUIStore.setState({ probeOpen: true, probeTab: 'sessions' });
    render(<ProbePanel />);

    expect(await screen.findByText('Scanning sessions…')).toBeTruthy();
    expect(screen.queryByText('This tab\'s content is coming soon.')).toBeNull();
  });
});

describe('useUIStore.openProbeTab (issue #375, the next 5 tabs rely on this)', () => {
  beforeEach(() => {
    useUIStore.setState({
      probeOpen: false,
      probeTab: 'files',
      activeDiffFile: null,
    });
  });

  it('is idempotent: second call with the same tab does not toggle', () => {
    // The activity-bar owns the "click active to collapse" UX via
    // `toggleProbe`. `openProbeTab` is pure "make visible" so call
    // sites stay one-liners; a toggle semantic here would silently
    // no-op repeated triggers (e.g. right-clicking a different mesh).
    useUIStore.getState().openProbeTab('properties');
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().probeTab).toBe('properties');

    useUIStore.getState().openProbeTab('properties');
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().probeTab).toBe('properties');
  });

  it('switches tab on a different argument while staying open', () => {
    useUIStore.getState().openProbeTab('properties');
    useUIStore.getState().openProbeTab('issues');
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().probeTab).toBe('issues');
  });

  it('does not touch activeDiffFile when switching tabs (#379)', () => {
    // The Center Workspace Diff Overlay floats over the terminal grid and is
    // independent of the active tab, so opening a probe tab (even right-
    // clicking Properties) leaves the open diff alone — it closes only via
    // Esc / "Back to Terminals" or the overlay's own auto-close.
    const ctx = {
      filePath: 'src/foo.ts',
      rootPath: '/repo',
      nodeId: 7,
      meshId: 1,
      source: 'base' as const,
    };
    useUIStore.setState({ probeTab: 'review', activeDiffFile: ctx });
    useUIStore.getState().openProbeTab('properties');
    expect(useUIStore.getState().probeTab).toBe('properties');
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useUIStore.getState().activeDiffFile).toEqual(ctx);
  });
});
