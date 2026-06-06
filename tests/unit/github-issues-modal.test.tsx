import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { GitHubIssuesModal } from '../../src/components/GitHubIssues/GitHubIssuesModal';
import type { ProviderEntry } from '../../src/components/Sidebar/ProviderDropdown';

// The modal calls useAgentNodeStore.getState().fetchAgentNodes() after a
// successful spawn. Stub that single method so we don't pull the real store
// (and its Terminal.tsx import chain) into this test.
vi.mock('../../src/stores/agentNodeStore', () => ({
  useAgentNodeStore: {
    getState: () => ({
      fetchAgentNodes: vi.fn().mockResolvedValue(undefined),
    }),
  },
}));

const ISSUES = [
  { number: 101, title: 'Add dark mode', body: 'body of #101' },
  { number: 102, title: 'Refactor auth', body: '' },
];

const PROVIDERS: ProviderEntry[] = [
  { id: 'anthropic', label: 'Anthropic', color: 'bg-blue-500' },
  { id: 'minimax', label: 'Minimax', color: 'bg-indigo-500' },
  { id: 'kimi', label: 'Kimi', color: 'bg-cyan-500' },
  { id: 'agy', label: 'Agy', color: 'bg-emerald-500' },
  { id: 'opencode', label: 'OpenCode', color: 'bg-amber-500' },
  { id: 'codex', label: 'Codex', color: 'bg-gray-500' },
];

function mockIssues(issues = ISSUES) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'get_repo_issues') return Promise.resolve(issues);
    if (cmd === 'spawn_issue_agent') return Promise.resolve({ id: 999, mesh_id: 1 });
    return Promise.resolve({});
  });
}

function setup(overrides: {
  defaultProvider?: string;
  onClose?: () => void;
} = {}) {
  const onClose = overrides.onClose ?? vi.fn();
  const getDefaultProvider = vi.fn().mockResolvedValue(overrides.defaultProvider ?? 'anthropic');
  render(
    <GitHubIssuesModal
      meshId={1}
      meshPath="/tmp/mesh"
      providerList={PROVIDERS}
      getDefaultProvider={getDefaultProvider}
      onClose={onClose}
    />,
  );
  return { onClose, getDefaultProvider };
}

describe('GitHubIssuesModal split spawn button', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('renders one Spawn button per issue and no dropdowns initially', async () => {
    mockIssues();
    setup();

    const buttons = await screen.findAllByRole('button', { name: /Spawn/ });
    expect(buttons).toHaveLength(ISSUES.length);
    // Dropdown menu items should not be visible.
    expect(screen.queryByRole('button', { name: 'Minimax' })).toBeNull();
  });

  it('uses the mesh default provider when the primary Spawn button is clicked', async () => {
    mockIssues([ISSUES[0]]);
    const { onClose, getDefaultProvider } = setup({ defaultProvider: 'kimi' });

    const spawnBtn = await screen.findByRole('button', { name: 'Spawn' });
    await userEvent.click(spawnBtn);

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'spawn_issue_agent',
        { meshId: 1, issueNumber: 101, issueTitle: 'Add dark mode', provider: 'kimi' },
      );
    });
    expect(getDefaultProvider).toHaveBeenCalledWith(1);
    // Spawn closes the modal on success.
    expect(onClose).toHaveBeenCalled();
  });

  it('opens the provider dropdown when the chevron is clicked', async () => {
    mockIssues([ISSUES[0]]);
    setup();

    const chevron = await screen.findByTitle('Choose provider');
    await userEvent.click(chevron);

    // All providers (no resume filter — this is a fresh spawn) are listed.
    expect(await screen.findByRole('button', { name: 'Anthropic' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Minimax' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Agy' })).toBeTruthy();
  });

  it('spawns with the chosen provider when a dropdown option is clicked', async () => {
    mockIssues([ISSUES[0]]);
    const { onClose, getDefaultProvider } = setup({ defaultProvider: 'anthropic' });

    // Open the dropdown for issue #101.
    await userEvent.click(await screen.findByTitle('Choose provider'));
    // Pick the Agy provider from the dropdown.
    await userEvent.click(await screen.findByRole('button', { name: 'Agy' }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'spawn_issue_agent',
        { meshId: 1, issueNumber: 101, issueTitle: 'Add dark mode', provider: 'agy' },
      );
    });
    // When the user picks explicitly, we don't need to ask the backend for the default.
    expect(getDefaultProvider).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });

  it('only opens the dropdown for the issue whose chevron was clicked', async () => {
    mockIssues();
    setup();

    const chevrons = await screen.findAllByTitle('Choose provider');
    expect(chevrons).toHaveLength(ISSUES.length);
    await userEvent.click(chevrons[0]);

    // Issue #101's dropdown is open — its provider options are visible.
    expect(await screen.findByRole('button', { name: 'Agy' })).toBeTruthy();
    // Each issue's row is in the DOM, but only the currently-open dropdown renders
    // its provider buttons. We assert by re-querying the full provider list — if
    // both dropdowns were open, every provider would appear once per row. We check
    // a single provider button appears exactly once.
    expect(screen.getAllByRole('button', { name: 'Agy' })).toHaveLength(1);
  });

  it('clears the spawning state when spawn fails, so the user can retry', async () => {
    mockIssues([ISSUES[0]]);
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.resolve([ISSUES[0]]);
      if (cmd === 'spawn_issue_agent') return Promise.reject(new Error('boom'));
      return Promise.resolve({});
    });
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const { onClose } = setup();

    await userEvent.click(await screen.findByRole('button', { name: 'Spawn' }));

    // Modal should still be open and the Spawn button should re-enable.
    await waitFor(() => {
      expect(onClose).not.toHaveBeenCalled();
    });
    expect((await screen.findByRole('button', { name: 'Spawn' })).hasAttribute('disabled')).toBe(false);
    consoleError.mockRestore();
  });

  it('survives a getDefaultProvider rejection without leaving the UI stuck', async () => {
    // Regression: the primary Spawn button awaits getDefaultProvider before
    // spawnIssueAgent. If the IPC call rejects, we must still reset spawning
    // and leave the modal open so the user can retry.
    mockIssues([ISSUES[0]]);
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const getDefaultProvider = vi.fn().mockRejectedValue(new Error('mesh store down'));
    const onClose = vi.fn();
    render(
      <GitHubIssuesModal
        meshId={1}
        meshPath="/tmp/mesh"
        providerList={PROVIDERS}
        getDefaultProvider={getDefaultProvider}
        onClose={onClose}
      />,
    );

    await userEvent.click(await screen.findByRole('button', { name: 'Spawn' }));

    await waitFor(() => {
      expect(getDefaultProvider).toHaveBeenCalledWith(1);
    });
    // Modal must stay open and the button must re-enable for retry.
    await waitFor(() => {
      expect(onClose).not.toHaveBeenCalled();
    });
    expect((await screen.findByRole('button', { name: 'Spawn' })).hasAttribute('disabled')).toBe(false);
    consoleError.mockRestore();
  });
});
