import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { GitHubIssuesModal } from '../../src/components/GitHubIssues/GitHubIssuesModal';
import type { ProviderEntry } from '../../src/components/Sidebar/ProviderDropdown';

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

/// A minimal IssueNodeDraft that satisfies the TypeScript return type. The
/// real Rust backend returns the full AgentNode row + the prefill string;
/// these tests only assert the call shape and the modal's UI behavior, so
/// the minimal shape is enough.
const DRAFT = {
  id: 999,
  mesh_id: 1,
  status: 'pending',
  prefill: 'Please work on GitHub issue #101 — Add dark mode\nhttps://github.com/example/repo/issues/101',
};

function mockIssues(issues = ISSUES) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'get_repo_issues') return Promise.resolve(issues);
    if (cmd === 'create_issue_node') return Promise.resolve(DRAFT);
    if (cmd === 'start_node_background') return Promise.resolve(undefined);
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

    // Two-stage flow: stage-1 (create_issue_node) is awaited; stage-2
    // (start_node_background) is fire-and-forget. The modal closes as
    // soon as stage-1 returns.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        'create_issue_node',
        { meshId: 1, issueNumber: 101, issueTitle: 'Add dark mode', provider: 'kimi' },
      );
    });
    expect(getDefaultProvider).toHaveBeenCalledWith(1);
    // Stage-2 was fired (fire-and-forget) with the prefill from stage-1.
    expect(invoke).toHaveBeenCalledWith(
      'start_node_background',
      { nodeId: DRAFT.id, prefill: DRAFT.prefill },
    );
    // Spawn closes the modal on stage-1 success.
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
        'create_issue_node',
        { meshId: 1, issueNumber: 101, issueTitle: 'Add dark mode', provider: 'agy' },
      );
    });
    // When the user picks explicitly, we don't need to ask the backend for the default.
    expect(getDefaultProvider).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
    // Stage-2 also fired.
    expect(invoke).toHaveBeenCalledWith(
      'start_node_background',
      { nodeId: DRAFT.id, prefill: DRAFT.prefill },
    );
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

  it('clears the spawning state when stage-1 fails, so the user can retry', async () => {
    // Regression: if stage-1 (create_issue_node) rejects, the modal
    // must stay open and the Spawn button must re-enable. Stage-2 is
    // never fired in that case (we never have a draft.id to pass).
    mockIssues([ISSUES[0]]);
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.resolve([ISSUES[0]]);
      if (cmd === 'create_issue_node') return Promise.reject(new Error('boom'));
      return Promise.resolve({});
    });
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    const { onClose } = setup();

    await userEvent.click(await screen.findByRole('button', { name: 'Spawn' }));

    // Modal should still be open and the Spawn button should re-enable.
    await waitFor(() => {
      expect(onClose).not.toHaveBeenCalled();
    });
    // Stage-2 must not have been fired — without a draft we have no
    // prefill to send.
    expect(invoke).not.toHaveBeenCalledWith('start_node_background', expect.anything());
    expect((await screen.findByRole('button', { name: 'Spawn' })).hasAttribute('disabled')).toBe(false);
    consoleError.mockRestore();
  });

  it('survives a getDefaultProvider rejection without leaving the UI stuck', async () => {
    // Regression: the primary Spawn button awaits getDefaultProvider before
    // create_issue_node. If the IPC call rejects, we must still reset spawning
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
    // Stage-1 was never reached, so stage-2 was never fired either.
    expect(invoke).not.toHaveBeenCalledWith('start_node_background', expect.anything());
    expect((await screen.findByRole('button', { name: 'Spawn' })).hasAttribute('disabled')).toBe(false);
    consoleError.mockRestore();
  });

  it('closes the modal after stage-1 even when stage-2 is in flight', async () => {
    // The whole point of the two-stage refactor: the modal must close
    // as soon as stage-1 returns, not when stage-2 finishes. To prove
    // this we make `start_node_background` a never-resolving promise —
    // any code path that awaited stage-2 would hang this test. The
    // assertion is then: onClose fires despite stage-2 still pending,
    // and start_node_background was invoked exactly once with the
    // draft from stage-1.
    mockIssues([ISSUES[0]]);
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_repo_issues') return Promise.resolve([ISSUES[0]]);
      if (cmd === 'create_issue_node') return Promise.resolve(DRAFT);
      if (cmd === 'start_node_background') return new Promise(() => {}); // never resolves
      return Promise.resolve({});
    });
    const { onClose } = setup();

    await userEvent.click(await screen.findByRole('button', { name: 'Spawn' }));

    // Modal closed — proves the React component did NOT await stage-2.
    await waitFor(() => {
      expect(onClose).toHaveBeenCalled();
    });
    // Stage-2 was fired exactly once, with the prefill from stage-1.
    const startCalls = vi.mocked(invoke).mock.calls.filter(
      ([cmd]) => cmd === 'start_node_background',
    );
    expect(startCalls).toHaveLength(1);
    expect(startCalls[0][1]).toEqual({ nodeId: DRAFT.id, prefill: DRAFT.prefill });
  });
});
