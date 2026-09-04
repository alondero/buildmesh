/**
 * PR pill merge menu — clicking the agent-node title PR pill offers
 * merge options instead of opening the browser directly.
 *
 * Desired UX (user request: reduce friction to merge PRs):
 *   - Pill click opens a menu (not the browser).
 *   - Menu has "Open on GitHub" + "Merge (squash & delete branch)".
 *   - Merge is gated behind an inline confirm (irreversible outward
 *     action, same contract as Probe's Pull Requests tab).
 *   - Draft PRs expose merge as aria-disabled (focusable, no action).
 *   - Failures keep the menu open with the error, which survives
 *     close/reopen until the next merge attempt.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';

const { openUrlMock, mergePrMock, invalidateMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn().mockResolvedValue(undefined),
  mergePrMock: vi.fn().mockResolvedValue('Merged'),
  invalidateMock: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: openUrlMock,
}));

vi.mock('../../src/lib/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/tauri')>();
  return { ...actual, mergePr: mergePrMock };
});

vi.mock('../../src/hooks/useOpenPr', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/hooks/useOpenPr')>();
  return { ...actual, invalidateOpenPrForNode: invalidateMock };
});

import { PrPill } from '../../src/components/AgentNodeView/PrPill';

const OPEN_PR = {
  number: 123,
  url: 'https://github.com/acme/demo/pull/123',
  title: 'Add PR chip',
  draft: false,
};

function openPillMenu() {
  fireEvent.click(screen.getByText('PR #123'));
}

function armConfirm() {
  openPillMenu();
  fireEvent.click(screen.getByText(/Merge \(squash/));
}

describe('PrPill merge menu', () => {
  beforeEach(() => {
    vi.stubGlobal('ResizeObserver', class {
      observe() {}
      disconnect() {}
    });
    openUrlMock.mockClear();
    mergePrMock.mockClear();
    mergePrMock.mockResolvedValue('Merged');
    invalidateMock.mockClear();
  });

  afterEach(() => vi.unstubAllGlobals());

  it('keeps the menu outside the clipping title and treats portaled clicks as inside', () => {
    const { container } = render(
      <div style={{ overflow: 'hidden', height: 24 }}>
        <PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />
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
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
    expect(screen.getByText('PR #123')).toBeTruthy();
  });

  it('clicking the pill opens a menu instead of opening the browser directly', () => {
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
    openPillMenu();
    expect(openUrlMock).not.toHaveBeenCalled();
    expect(screen.getByRole('menu')).toBeTruthy();
    expect(screen.getByText(/Open on GitHub/)).toBeTruthy();
    expect(screen.getByText(/Merge/)).toBeTruthy();
  });

  it('wires the trigger to the menu for assistive tech', () => {
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
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
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
    openPillMenu();
    fireEvent.click(screen.getByText(/Open on GitHub/));
    expect(openUrlMock).toHaveBeenCalledWith(OPEN_PR.url);
  });

  it('merge requires confirm then calls mergePr and invalidates the chip cache', async () => {
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
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
    expect(invalidateMock).toHaveBeenCalledWith(1, '/repo');
    // Menu closes on success.
    await waitFor(() => {
      expect(screen.queryByRole('menu')).toBeNull();
    });
  });

  it('cancel backs out of the confirm without merging', () => {
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
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
    // Open (0) -> ArrowDown to Merge (1) -> ArrowDown to Cancel (2).
    // Activating Cancel must NOT drop focus to document.body, and the
    // post-cancel Merge row must carry tabIndex="0" so the WAI-ARIA
    // roving-tabindex invariant holds after the slot shrinks.
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
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
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
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
    const { unmount } = render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
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
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
    armConfirm();
    fireEvent.click(
      screen.getByLabelText(`Confirm squash merge of pull request #${OPEN_PR.number}`),
    );

    // Merging row replaces Confirm/Cancel: still exactly 2 menuitems,
    // matching itemCount, so no arrow step can fall off the end.
    expect(screen.getByText('Merging…')).toBeTruthy();
    expect(screen.getAllByRole('menuitem')).toHaveLength(2);
    expect(
      screen.getByLabelText(`Open pull request #${OPEN_PR.number} on GitHub`)
        .getAttribute('aria-disabled'),
    ).toBe('true');

    // Walk past the end — focus must wrap inside the menu, never void.
    const items = screen.getAllByRole('menuitem');
    expect(document.activeElement).toBe(items[0]);
    fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(items[1]);
    fireEvent.keyDown(document.activeElement!, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(items[0]);

    resolveMerge('Merged');
    await waitFor(() => {
      expect(screen.queryByRole('menu')).toBeNull();
    });
  });

  it('aria-disabled merge row for drafts stays focusable but never merges', () => {
    render(
      <PrPill nodeId={1} gitPath="/repo" openPr={{ ...OPEN_PR, draft: true }} />,
    );
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
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
    const trigger = screen.getByTestId('pr-pill-trigger');
    openPillMenu();

    const items = screen.getAllByRole('menuitem');
    expect(items).toHaveLength(2);
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
        <PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />
      </div>,
    );
    openPillMenu();
    expect(screen.getByRole('menu')).toBeTruthy();
    fireEvent.mouseDown(screen.getByTestId('outside'));
    expect(screen.queryByRole('menu')).toBeNull();
  });
});
