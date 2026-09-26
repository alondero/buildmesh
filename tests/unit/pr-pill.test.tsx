/**
 * PR pill menu — clicking the agent-node title PR pill offers merge options
 * instead of opening the browser directly, plus a "Spawn reviewer agent" row
 * that launches a PR-probe-style agent in its own worktree and groups it onto
 * this node's card as another Node Activity tab.
 *
 * Desired UX (user request: reduce friction to merge PRs):
 *   - Pill click opens a menu (not the browser).
 *   - Menu has "Open on GitHub" + "Merge (squash & delete branch)".
 *   - Merge is gated behind an inline confirm (irreversible outward
 *     action, same contract as Probe's Pull Requests tab).
 *   - Draft PRs expose merge as aria-disabled (focusable, no action).
 *   - Failures keep the menu open with the error, which survives
 *     close/reopen until the next merge attempt.
 *   - "Spawn reviewer agent…" reuses the shared Spawn Menu for the provider
 *     options and does what the PRs probe's `+` does (`create_pr_node`), with
 *     `reviewer: true` so the backend gives it its own worktree, then groups
 *     the new node onto the clicked node's card.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';

const { openUrlMock, mergePrMock, invalidateMock, createPrNodeMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn().mockResolvedValue(undefined),
  mergePrMock: vi.fn().mockResolvedValue('Merged'),
  invalidateMock: vi.fn(),
  createPrNodeMock: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: openUrlMock,
}));

vi.mock('../../src/lib/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/tauri')>();
  return { ...actual, mergePr: mergePrMock, createPrNode: createPrNodeMock };
});

vi.mock('../../src/hooks/useOpenPr', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/hooks/useOpenPr')>();
  return { ...actual, invalidateOpenPrForNode: invalidateMock };
});

import { PrPill } from '../../src/components/AgentNodeView/PrPill';
import type { OpenPr } from '../../src/types/generated/OpenPr';
import type { AgentNode } from '../../src/stores/agentNodeStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useNodeActivityStore } from '../../src/stores/nodeActivityStore';
import type { SpawnOption } from '../../src/lib/groups';

const OPEN_PR: OpenPr = {
  number: 123,
  url: 'https://github.com/acme/demo/pull/123',
  title: 'Add PR chip',
  draft: false,
  head_ref: 'feat/pr-chip',
  head_sha: 'abcdef0123456789abcdef0123456789abcdef01',
  head_repo_owner: 'acme',
  head_repo_clone_url: 'https://github.com/acme/demo.git',
};

const NODE_ID = 1;
const MESH_ID = 7;

function spawnOption(id: string, label: string): SpawnOption {
  return {
    id,
    label,
    icon: id,
    harness_id: id,
    provider_id: null,
    is_proxied: false,
    group_key: id,
    color: '#fff',
  };
}

const PROVIDERS: SpawnOption[] = [spawnOption('claude', 'Claude Code'), spawnOption('codex', 'Codex')];

/** The draft `create_pr_node` hands back — flattened `AgentNode` + `prefill`. */
const REVIEWER_DRAFT = {
  id: 555,
  mesh_id: MESH_ID,
  name: 'pr123-review-add-pr-chip',
  status: 'pending',
  provider: 'claude',
  source_pr: 123,
  prefill: 'Review PR #123',
} as unknown as AgentNode;

function renderPill(props: Partial<React.ComponentProps<typeof PrPill>> = {}) {
  return render(
    <PrPill nodeId={NODE_ID} meshId={MESH_ID} gitPath="/repo" openPr={OPEN_PR} providers={PROVIDERS} {...props} />,
  );
}

function openPillMenu() {
  fireEvent.click(screen.getByText('PR #123'));
}

function armConfirm() {
  openPillMenu();
  fireEvent.click(screen.getByText(/Merge \(squash/));
}

describe('PrPill merge menu', () => {
  beforeEach(() => {
    openUrlMock.mockClear();
    mergePrMock.mockClear();
    mergePrMock.mockResolvedValue('Merged');
    invalidateMock.mockClear();
    createPrNodeMock.mockReset();
    createPrNodeMock.mockResolvedValue(REVIEWER_DRAFT);
    // The reviewer spawn groups onto the clicked node's card, so both stores
    // are real (not mocked) — seed the clicked node and clear view state.
    useAgentNodeStore.setState({
      nodesById: { [NODE_ID]: { id: NODE_ID, mesh_id: MESH_ID, name: 'impl', status: 'running' } as AgentNode },
      nodeIds: [NODE_ID],
      activeNodeId: null,
    });
    useNodeActivityStore.setState({ groups: [], selections: {}, utilities: {} });
  });

  it('keeps the menu outside the clipping title and treats portaled clicks as inside', () => {
    const { container } = render(
      <div style={{ overflow: 'hidden', height: 24 }}>
        <PrPill nodeId={NODE_ID} meshId={MESH_ID} gitPath="/repo" openPr={OPEN_PR} providers={PROVIDERS} />
      </div>,
    );
    openPillMenu();
    const menu = screen.getByRole('menu');
    expect(container.contains(menu)).toBe(false);
    expect(menu.parentElement).toBe(document.body);
    fireEvent.mouseDown(screen.getByRole('menuitem', { name: 'Merge pull request #123', exact: true }));
    expect(screen.getByRole('menu')).toBe(menu);
  });

  it('renders the PR number pill', () => {
    renderPill();
    expect(screen.getByText('PR #123')).toBeTruthy();
  });

  it('collapses to an icon trigger without the PR number label', () => {
    renderPill({ compact: true });
    expect(screen.queryByText('PR #123')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Open pull request #123 options' }));
    expect(screen.getByRole('menu')).toBeTruthy();
  });

  it('clicking the pill opens a menu instead of opening the browser directly', () => {
    renderPill();
    openPillMenu();
    expect(openUrlMock).not.toHaveBeenCalled();
    expect(screen.getByRole('menu')).toBeTruthy();
    expect(screen.getByText(/Open on GitHub/)).toBeTruthy();
    expect(screen.getByText(/Merge/)).toBeTruthy();
  });

  it('wires the trigger to the menu for assistive tech', () => {
    renderPill();
    const trigger = screen.getByTestId('pr-pill-trigger');
    expect(trigger.getAttribute('aria-haspopup')).toBe('menu');
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
    openPillMenu();
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
    const controls = trigger.getAttribute('aria-controls');
    expect(controls).toBeTruthy();
    expect(screen.getByRole('menu').getAttribute('id')).toBe(controls);
  });

  it('menu "Open on GitHub" opens the PR url', () => {
    renderPill();
    openPillMenu();
    fireEvent.click(screen.getByText(/Open on GitHub/));
    expect(openUrlMock).toHaveBeenCalledWith(OPEN_PR.url);
  });

  it('merge requires confirm then calls mergePr and invalidates the chip cache', async () => {
    renderPill();
    armConfirm();

    // First click arms confirm — must NOT merge yet.
    expect(mergePrMock).not.toHaveBeenCalled();
    const confirmBtn = screen.getByLabelText(
      `Confirm squash merge of pull request #${OPEN_PR.number}`,
    );
    fireEvent.click(confirmBtn);

    await waitFor(() => {
      expect(mergePrMock).toHaveBeenCalledWith(OPEN_PR.url);
    });
    expect(invalidateMock).toHaveBeenCalledWith(NODE_ID, '/repo');
    // Menu closes on success.
    await waitFor(() => {
      expect(screen.queryByRole('menu')).toBeNull();
    });
  });

  it('cancel backs out of the confirm without merging', () => {
    renderPill();
    armConfirm();
    fireEvent.click(
      screen.getByLabelText(`Cancel merge of pull request #${OPEN_PR.number}`),
    );
    expect(mergePrMock).not.toHaveBeenCalled();
    // Menu stays open, back on the plain Merge row (no Confirm).
    expect(screen.getByRole('menu')).toBeTruthy();
    expect(screen.getByText(/Merge \(squash/)).toBeTruthy();
    expect(
      screen.queryByLabelText(`Confirm squash merge of pull request #${OPEN_PR.number}`),
    ).toBeNull();
  });

  it('cancelling from the keyboard-driven Cancel row restores focus and the roving tabindex', async () => {
    // Drive the keyboard path that landed Cancel at activeIndex=2:
    // Open (0) -> ArrowDown to Merge/Confirm (1) -> ArrowDown to Cancel (2).
    // Activating Cancel must NOT drop focus to document.body, and the
    // post-cancel Merge row must carry tabIndex="0" so the WAI-ARIA
    // roving-tabindex invariant holds after the slot shrinks.
    renderPill();
    armConfirm();
    const cancel = screen.getByLabelText(
      `Cancel merge of pull request #${OPEN_PR.number}`,
    );
    fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
    fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(cancel);
    // Native button: activating Cancel triggers the onClick. The
    // WAI-ARIA Enter / Space activation contract is a real-browser
    // behaviour; React's fireEvent.keyDown does not synthesise the
    // synthesized click on the focused button, so we activate it the
    // way a screen reader / Space-key user would — by clicking it.
    fireEvent.click(cancel);

    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => {
        const mergeItem = screen.getByLabelText(
          `Merge pull request #${OPEN_PR.number}`,
        );
        const openItem = screen.getByLabelText(
          `Open pull request #${OPEN_PR.number} on GitHub`,
        );
        // Merge (1) is the active slot — focus + tabIndex=0.
        expect(document.activeElement).toBe(mergeItem);
        expect(mergeItem.getAttribute('tabindex')).toBe('0');
        expect(openItem.getAttribute('tabindex')).toBe('-1');
        expect(mergePrMock).not.toHaveBeenCalled();
        resolve();
      }),
    );
  });

  it('a merge failure keeps the menu open with the error and resets the confirm', async () => {
    mergePrMock.mockRejectedValueOnce(new Error('Conflicts'));
    renderPill();
    armConfirm();
    fireEvent.click(
      screen.getByLabelText(`Confirm squash merge of pull request #${OPEN_PR.number}`),
    );

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('Conflicts');
    });
    // Menu stays open for a retry, confirm reset to the Merge row.
    expect(screen.getByRole('menu')).toBeTruthy();
    expect(screen.getByText(/Merge \(squash/)).toBeTruthy();
    expect(
      screen.queryByLabelText(`Confirm squash merge of pull request #${OPEN_PR.number}`),
    ).toBeNull();
    expect(invalidateMock).not.toHaveBeenCalled();
  });

  it('a merge error survives close and reopen until the next attempt', async () => {
    mergePrMock.mockRejectedValueOnce(new Error('Blocked'));
    const { unmount } = renderPill();
    armConfirm();
    fireEvent.click(
      screen.getByLabelText(`Confirm squash merge of pull request #${OPEN_PR.number}`),
    );
    await waitFor(() => {
      expect(screen.getByRole('alert')).toBeTruthy();
    });

    // Dismiss and reopen — the error must still be readable.
    fireEvent.click(screen.getByText('PR #123'));
    expect(screen.queryByRole('menu')).toBeNull();
    openPillMenu();
    expect(screen.getByRole('alert').textContent).toContain('Blocked');
    unmount();
  });

  it('while merging, Open is parked and arrow navigation stays inside the menu', async () => {
    let resolveMerge!: (v: string) => void;
    mergePrMock.mockImplementationOnce(
      () => new Promise<string>((res) => { resolveMerge = res; }),
    );
    renderPill();
    armConfirm();
    fireEvent.click(
      screen.getByLabelText(`Confirm squash merge of pull request #${OPEN_PR.number}`),
    );

    // Merging row replaces Confirm/Cancel: Open + Merging + Spawn — three
    // menuitems, matching itemCount, so no arrow step can fall off the end.
    expect(screen.getByText('Merging…')).toBeTruthy();
    const items = screen.getAllByRole('menuitem');
    expect(items).toHaveLength(3);
    expect(
      screen.getByLabelText(`Open pull request #${OPEN_PR.number} on GitHub`)
        .getAttribute('aria-disabled'),
    ).toBe('true');
    // The spawn row is parked too — opening the dialog mid-merge would unmount
    // it when a successful merge invalidates the chip.
    expect(screen.getByTestId('pr-spawn-reviewer').getAttribute('aria-disabled')).toBe('true');

    // Walk past the end — focus must wrap inside the menu, never void.
    expect(document.activeElement).toBe(items[0]);
    fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(items[1]);
    fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(items[2]);
    fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(items[0]);

    resolveMerge('Merged');
    await waitFor(() => {
      expect(screen.queryByRole('menu')).toBeNull();
    });
  });

  it('aria-disabled merge row for drafts stays focusable but never merges', () => {
    renderPill({ openPr: { ...OPEN_PR, draft: true } });
    openPillMenu();
    const mergeBtn = screen.getByText(/Merge \(squash/);
    const btn = mergeBtn.closest('button')!;
    // Roving-tabindex contract: focusable (has a tabIndex slot), but
    // marked aria-disabled instead of native-disabled so ArrowDown
    // can still land on it per the WAI-ARIA menu pattern.
    expect(btn.getAttribute('aria-disabled')).toBe('true');
    expect(btn.hasAttribute('disabled')).toBe(false);
    expect(btn.hasAttribute('tabindex')).toBe(true);

    fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(btn);
    fireEvent.click(btn);
    expect(mergePrMock).not.toHaveBeenCalled();
    expect(screen.getByRole('menu')).toBeTruthy();
  });

  it('arrow navigation, Escape, and Tab follow the menu contract', async () => {
    renderPill();
    const trigger = screen.getByTestId('pr-pill-trigger');
    openPillMenu();

    const items = screen.getAllByRole('menuitem');
    expect(items).toHaveLength(3);
    expect(document.activeElement).toBe(items[0]);
    fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(items[1]);
    fireEvent.keyDown(document.activeElement!, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(items[0]);

    // Escape closes and returns focus to the trigger.
    fireEvent.keyDown(document.activeElement!, { key: 'Escape' });
    expect(screen.queryByRole('menu')).toBeNull();
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => {
        expect(document.activeElement).toBe(trigger);
        resolve();
      }),
    );

    // Tab leaves and closes without a focus trap.
    openPillMenu();
    expect(screen.getByRole('menu')).toBeTruthy();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Tab' });
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('mousedown outside the menu dismisses it', () => {
    render(
      <div>
        <button data-testid="outside">outside</button>
        <PrPill nodeId={NODE_ID} meshId={MESH_ID} gitPath="/repo" openPr={OPEN_PR} providers={PROVIDERS} />
      </div>,
    );
    openPillMenu();
    expect(screen.getByRole('menu')).toBeTruthy();
    fireEvent.mouseDown(screen.getByTestId('outside'));
    expect(screen.queryByRole('menu')).toBeNull();
  });
});

describe('PrPill reviewer spawn', () => {
  beforeEach(() => {
    createPrNodeMock.mockReset();
    createPrNodeMock.mockResolvedValue(REVIEWER_DRAFT);
    useAgentNodeStore.setState({
      nodesById: { [NODE_ID]: { id: NODE_ID, mesh_id: MESH_ID, name: 'impl', status: 'running' } as AgentNode },
      nodeIds: [NODE_ID],
      activeNodeId: null,
    });
    useNodeActivityStore.setState({ groups: [], selections: {}, utilities: {} });
  });

  function openReviewerDialog() {
    openPillMenu();
    fireEvent.click(screen.getByTestId('pr-spawn-reviewer'));
  }

  it('offers "Spawn reviewer agent…" which closes the pill menu and opens the shared Spawn Menu', () => {
    renderPill();
    openReviewerDialog();
    expect(screen.queryByRole('menu', { name: /Pull request #123 actions/ })).toBeNull();
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByRole('heading', { name: 'Spawn reviewer agent for PR #123' })).toBeTruthy();
    const menu = screen.getByRole('menu', { name: 'Select a provider' });
    expect(menu).toBeTruthy();
    expect(screen.getByRole('menuitem', { name: 'Claude Code' })).toBeTruthy();
  });

  it('picking a provider spawns the PR reviewer and groups it as a tab', async () => {
    renderPill();
    openReviewerDialog();
    fireEvent.click(screen.getByRole('menuitem', { name: 'Claude Code' }));

    await waitFor(() => {
      expect(createPrNodeMock).toHaveBeenCalledWith(
        MESH_ID,
        OPEN_PR.number,
        OPEN_PR.title,
        OPEN_PR.head_ref,
        OPEN_PR.head_sha,
        'claude',
        OPEN_PR.head_repo_owner,
        OPEN_PR.head_repo_clone_url,
        undefined,
        true,
      );
    });

    // The reviewer is merged into the clicked node's card and focused.
    await waitFor(() => {
      expect(useNodeActivityStore.getState().groups).toEqual([[NODE_ID, REVIEWER_DRAFT.id]]);
    });
    expect(useNodeActivityStore.getState().selections[NODE_ID]).toEqual({ nodeId: REVIEWER_DRAFT.id, utility: false });
    expect(useAgentNodeStore.getState().activeNodeId).toBe(REVIEWER_DRAFT.id);
    // Dialog closed on success.
    await waitFor(() => {
      expect(screen.queryByRole('dialog')).toBeNull();
    });
  });

  it('returns focus to the PR pill when the dialog is dismissed', async () => {
    renderPill();
    openReviewerDialog();
    // The Modal moves focus into the dialog on mount…
    expect(screen.getByRole('dialog').contains(document.activeElement)).toBe(true);

    fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    // …and the pill — not <body> — must own it again. The dialog's own Modal
    // restore cannot do this: the row that opened it unmounts in the same
    // commit, so its captured `previouslyFocused` is already detached.
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => {
        expect(document.activeElement).toBe(screen.getByTestId('pr-pill-trigger'));
        resolve();
      }),
    );
  });

  it('returns focus to the PR pill after a successful spawn', async () => {
    renderPill();
    openReviewerDialog();
    fireEvent.click(screen.getByRole('menuitem', { name: 'Claude Code' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => {
        expect(document.activeElement).toBe(screen.getByTestId('pr-pill-trigger'));
        resolve();
      }),
    );
  });

  it('shows an empty state instead of an empty menu when no harnesses are available', () => {
    renderPill({ providers: [] });
    openReviewerDialog();
    expect(screen.getByTestId('pr-reviewer-no-providers')).toBeTruthy();
    expect(screen.queryByRole('menu', { name: 'Select a provider' })).toBeNull();
  });

  it('keeps the dialog open with an error when the spawn fails', async () => {
    createPrNodeMock.mockRejectedValueOnce(new Error('PR head is unfetchable'));
    renderPill();
    openReviewerDialog();
    fireEvent.click(screen.getByRole('menuitem', { name: 'Claude Code' }));

    expect((await screen.findByRole('alert')).textContent).toContain('PR head is unfetchable');
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(useNodeActivityStore.getState().groups).toEqual([]);
  });
});
