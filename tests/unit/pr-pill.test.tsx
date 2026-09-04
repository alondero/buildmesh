/**
 * PR pill merge menu — clicking the agent-node title PR pill must offer
 * merge options instead of opening the browser directly.
 *
 * Desired UX (user request: reduce friction to merge PRs):
 *   - Pill click opens a menu (not the browser).
 *   - Menu has "Open on GitHub" + "Merge (squash & delete branch)".
 *   - Merge is gated behind an inline confirm (irreversible outward
 *     action, same contract as Probe's Pull Requests tab).
 *   - Draft PRs disable merge.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';

const { openUrlMock, mergePrMock, refreshMock } = vi.hoisted(() => ({
  openUrlMock: vi.fn().mockResolvedValue(undefined),
  mergePrMock: vi.fn().mockResolvedValue('Merged'),
  refreshMock: vi.fn(),
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
  return { ...actual, refreshOpenPrByPath: refreshMock };
});

import { PrPill } from '../../src/components/AgentNodeView/PrPill';

const OPEN_PR = {
  number: 123,
  url: 'https://github.com/acme/demo/pull/123',
  title: 'Add PR chip',
  draft: false,
};

describe('PrPill merge menu', () => {
  beforeEach(() => {
    openUrlMock.mockClear();
    mergePrMock.mockClear();
    refreshMock.mockClear();
  });

  it('renders the PR number pill', () => {
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
    expect(screen.getByText('PR #123')).toBeTruthy();
  });

  it('clicking the pill opens a menu instead of opening the browser directly', () => {
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
    fireEvent.click(screen.getByText('PR #123'));
    expect(openUrlMock).not.toHaveBeenCalled();
    expect(screen.getByRole('menu')).toBeTruthy();
    expect(screen.getByText(/Open on GitHub/)).toBeTruthy();
    expect(screen.getByText(/Merge/)).toBeTruthy();
  });

  it('menu "Open on GitHub" opens the PR url', () => {
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
    fireEvent.click(screen.getByText('PR #123'));
    fireEvent.click(screen.getByText(/Open on GitHub/));
    expect(openUrlMock).toHaveBeenCalledWith(OPEN_PR.url);
  });

  it('merge requires confirm then calls mergePr and refreshes the chip', async () => {
    render(<PrPill nodeId={1} gitPath="/repo" openPr={OPEN_PR} />);
    fireEvent.click(screen.getByText('PR #123'));
    fireEvent.click(screen.getByText(/Merge \(squash/));

    // First click arms confirm — must NOT merge yet.
    expect(mergePrMock).not.toHaveBeenCalled();
    const confirmBtn = screen.getByLabelText(
      `Confirm squash merge of pull request #${OPEN_PR.number}`,
    );
    fireEvent.click(confirmBtn);

    await waitFor(() => {
      expect(mergePrMock).toHaveBeenCalledWith(OPEN_PR.url);
    });
    expect(refreshMock).toHaveBeenCalledWith('/repo');
  });

  it('disables merge for draft PRs', () => {
    render(
      <PrPill nodeId={1} gitPath="/repo" openPr={{ ...OPEN_PR, draft: true }} />,
    );
    fireEvent.click(screen.getByText('PR #123'));
    const mergeBtn = screen.getByText(/Merge \(squash/);
    expect(mergeBtn.closest('button')?.hasAttribute('disabled')).toBe(true);
  });
});
