/**
 * Issue #778 — confirmation dialog for Regenerate on a running node.
 *
 * Interrupting a `running` agent with `regenerate_agent_node` drops the
 * agent's in-flight PTY output without warning. A picker-row click on
 * a `running` node must therefore open a confirmation dialog instead
 * of firing the IPC; clicking Confirm proceeds, clicking Cancel (or
 * Escape / backdrop) does nothing. For `idle` / `awaiting_input` /
 * `error` the dialog is skipped entirely — no live work to lose.
 *
 * Contract (pinned one assertion per issue bullet):
 *   1. running → dialog opens, IPC does NOT fire until Confirm
 *   2. idle | awaiting_input | error → IPC fires immediately, no dialog
 *   3. dialog renders two buttons ("Regenerate" confirm, "Cancel")
 *   4. dialog message includes the chosen provider label
 *   5. Confirm → IPC fires once with (nodeId, providerId); dialog closes
 *   6. Cancel → IPC does NOT fire; dialog closes
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, fireEvent, waitFor, cleanup } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { NodeItem } from '../../src/components/Sidebar/NodeItem';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { getMeshColor } from '../../src/lib/meshColors';
import type { SpawnOption } from '../../src/lib/groups';
import { colorClassForProvider } from '../../src/lib/groups';

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 42,
    mesh_id: 1,
    name: 'calm-sweet-wolf',
    path: '/repo',
    branch: 'main',
    env: 'wsl',
    provider: 'anthropic',
    status: 'running',
    cli_session_id: null,
    use_worktree: false,
    source_issue: null,
    source_pr: null,
    head_repo_owner: null,
    head_repo_clone_url: null,
    source_pr_pinned_sha: null,
    position: 0,
    created_at: '2026-01-01',
    ...overrides,
  };
}

function makeProvider(
  id: string,
  overrides: Partial<SpawnOption> = {},
): SpawnOption {
  return {
    id,
    label: id,
    icon: null,
    harness_id: id,
    provider_id: id,
    is_proxied: false,
    group_key: id,
    color: colorClassForProvider(id),
    ...overrides,
  };
}

const meshColor = getMeshColor(1);

function renderNode(node: AgentNode = makeNode(), providerList?: SpawnOption[]) {
  return render(
    <NodeItem
      node={node}
      meshColor={meshColor}
      isActive={false}
      providerList={providerList}
      onSelect={vi.fn()}
      onDelete={vi.fn()}
    />,
  );
}

function openContextMenu() {
  const row = screen.getByText('calm-sweet-wolf').closest('[data-session-item]')!;
  fireEvent.contextMenu(row, { clientX: 100, clientY: 200 });
}

/**
 * Open the Regenerate picker so the next picker-row click goes through
 * `pickProvider`. Mirrors `openSubmenu` in sidebar-node-item-context-menu.test.tsx.
 */
async function openPicker(node: AgentNode, providers: SpawnOption[]) {
  renderNode(node, providers);
  openContextMenu();
  const trigger = screen.getByText(/Regenerate/).closest('button')!;
  await userEvent.click(trigger);
  await waitFor(() => {
    expect(screen.getByTestId('regenerate-submenu')).toBeTruthy();
  });
}

describe('NodeItem running-node confirm dialog (issue #778)', () => {
  beforeEach(() => {
    useAgentNodeStore.setState({
      agentNodes: [],
      activeNodeId: null,
      loading: false,
      error: null,
      closingNodeIds: new Set(),
    });
    vi.spyOn(useAgentNodeStore.getState(), 'regenerateAgentNode').mockImplementation(
      async (nodeId, newProviderId) => {
        const existing = useAgentNodeStore.getState().agentNodes.find(n => n.id === nodeId);
        return { ...(existing ?? makeNode()), provider: newProviderId } as AgentNode;
      },
    );
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  // Bullet 1 — running: picker-row click opens the dialog, IPC stays quiet.
  it('opens a confirmation dialog when picking a provider on a running node', async () => {
    const node = makeNode({ id: 11, status: 'running', provider: 'anthropic' });
    await openPicker(node, [
      makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
      makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
    ]);

    await userEvent.click(screen.getByText(/Claude Code/));

    // Dialog rendered with the running-flavored message.
    const dialog = screen.getByRole('dialog');
    expect(dialog).toBeTruthy();
    expect(dialog.textContent).toMatch(/currently working/i);
    // And the IPC must NOT have fired yet — the dialog is the gate.
    expect(useAgentNodeStore.getState().regenerateAgentNode).not.toHaveBeenCalled();
  });

  // Bullet 2 — skip dialog for idle / awaiting_input / error.
  it.each(['idle', 'awaiting_input', 'error'] as const)(
    'skips the confirmation dialog and fires the IPC immediately when status is "%s"',
    async (status) => {
      const node = makeNode({ id: 21, status, provider: 'anthropic' });
      await openPicker(node, [
        makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
        makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
      ]);

      await userEvent.click(screen.getByText(/Claude Code/));

      // IPC fired once with the right args — no dialog gate for these statuses.
      expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledTimes(1);
      expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledWith(21, 'claude');
      // No dialog rendered (the picker closed after the click, and no modal took its place).
      expect(screen.queryByRole('dialog')).toBeNull();
    },
  );

  // Bullet 3 — dialog has exactly the two buttons the issue spec calls out.
  it('renders a Regenerate button and a Cancel button on the running-node dialog', async () => {
    const node = makeNode({ status: 'running', provider: 'anthropic' });
    await openPicker(node, [
      makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
      makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
    ]);

    await userEvent.click(screen.getByText(/Claude Code/));

    const dialog = screen.getByRole('dialog');
    // Two buttons inside the dialog, with the exact labels the issue calls for.
    const buttons = dialog.querySelectorAll('button');
    const labels = Array.from(buttons).map((b) => b.textContent?.trim());
    expect(labels).toContain('Regenerate');
    expect(labels).toContain('Cancel');
  });

  // Bullet 4 — message interpolates the chosen provider label so the user
  // sees exactly which Model Provider they're switching to.
  it('interpolates the chosen provider label into the dialog message', async () => {
    const node = makeNode({ status: 'running', provider: 'anthropic' });
    await openPicker(node, [
      makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
      makeProvider('claude', {
        label: 'Claude Code · Minimax',
        group_key: 'claude',
        harness_id: 'claude',
      }),
    ]);

    await userEvent.click(screen.getByText(/Claude Code · Minimax/));

    expect(screen.getByRole('dialog').textContent).toMatch(/Claude Code · Minimax/);
    expect(useAgentNodeStore.getState().regenerateAgentNode).not.toHaveBeenCalled();
  });

  // Bullet 5 — Confirm proceeds with the IPC and closes the dialog.
  it('fires regenerate_agent_node and closes the dialog when Confirm is clicked', async () => {
    const node = makeNode({ id: 33, status: 'running', provider: 'anthropic' });
    await openPicker(node, [
      makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
      makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
    ]);

    await userEvent.click(screen.getByText(/Claude Code/));
    expect(screen.getByRole('dialog')).toBeTruthy();

    // Click the Regenerate button inside the dialog (the picker row is
    // already gone — the picker closed on click — so the only "Regenerate"
    // button left is the dialog confirm).
    await userEvent.click(screen.getByRole('button', { name: 'Regenerate' }));

    await waitFor(() => {
      expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledTimes(1);
    });
    expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledWith(33, 'claude');
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).toBeNull();
    });
  });

  // Bullet 6 — Cancel does nothing and closes the dialog.
  it('does NOT fire regenerate_agent_node and closes the dialog when Cancel is clicked', async () => {
    const node = makeNode({ id: 44, status: 'running', provider: 'anthropic' });
    await openPicker(node, [
      makeProvider('anthropic', { group_key: 'anthropic', harness_id: 'anthropic' }),
      makeProvider('claude', { label: 'Claude Code', group_key: 'claude', harness_id: 'claude' }),
    ]);

    await userEvent.click(screen.getByText(/Claude Code/));
    expect(screen.getByRole('dialog')).toBeTruthy();

    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).toBeNull();
    });
    expect(useAgentNodeStore.getState().regenerateAgentNode).not.toHaveBeenCalled();
  });
});
