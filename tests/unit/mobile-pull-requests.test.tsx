import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import PullRequestsScreen, { NodePullRequest } from '../../src/mobile/screens/PullRequests';
import type { GitHubPullRequest } from '../../src/types/generated/GitHubPullRequest';
import type { GitHubPullRequestFeed } from '../../src/types/generated/GitHubPullRequestFeed';
import type { PrMergeability } from '../../src/types/generated/PrMergeability';
import type { Mesh } from '../../src/mobile/api';

const pr: GitHubPullRequest = {
  number: 42, title: 'Fix search', body: '', url: 'https://github.com/o/r/pull/42', state: 'open', draft: false,
  head_ref: 'feature/search', head_repo_owner: 'o', head_repo_clone_url: 'https://github.com/o/r.git',
  head_sha: 'abc', author: 'o', mergeable: true, mergeable_state: 'clean', checks_state: 'success', review_decision: 'approved',
};
function feed(items = [pr]): GitHubPullRequestFeed {
  return { items, completeness: { returned: items.length, pages_fetched: 1, complete: true, incomplete_reason: null, reported_total: null } };
}
const response = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
let current: GitHubPullRequestFeed;
let detail: PrMergeability;
let fetcher: ReturnType<typeof vi.fn>;
beforeEach(() => {
  current = feed(); detail = { mergeable: true, mergeable_state: 'clean' };
  fetcher = vi.fn((url: string) => Promise.resolve(response(
    url.includes('/git/branch') ? { branch: 'feature/search' } : url.includes('/mergeability') ? detail :
      url.endsWith('/merge') ? { url: pr.url, pr_number: 42 } : current,
  )));
  vi.stubGlobal('fetch', fetcher);
});
afterEach(() => { vi.unstubAllGlobals(); });
const nodePanel = (auth = vi.fn()) => <NodePullRequest meshId={3} nodeId={17} onAuthFailed={auth} />;

it('matches the live worktree branch, excludes forks with the same branch, and shows checks/reviews', async () => {
  current = feed([{ ...pr, number: 9, head_ref: 'main' }, { ...pr, number: 10, head_repo_clone_url: 'https://github.com/fork/r.git' }, pr]);
  render(nodePanel());
  expect(await screen.findByText('PR #42: Fix search')).toBeTruthy();
  expect(screen.queryByText('PR #9: Fix search')).toBeNull();
  expect(screen.queryByText('PR #10: Fix search')).toBeNull();
  expect(screen.getByText('Checks: passing')).toBeTruthy();
  expect(screen.getByText('Review: approved')).toBeTruthy();
});

it.each([
  { draft: true }, { mergeable: null }, { mergeable: false }, { mergeable_state: 'blocked' },
  { mergeable_state: 'behind' }, { mergeable_state: 'unknown' }, { checks_state: 'pending' },
  { checks_state: 'failure' }, { review_decision: 'changes_requested' }, { review_decision: 'review_required' },
])('disables merge for %j', async patch => {
  current = feed([{ ...pr, ...patch }]);
  render(nodePanel());
  expect((await screen.findByRole('button', { name: 'Squash and merge' }) as HTMLButtonElement).disabled).toBe(true);
});

it('requires confirmation, rechecks mergeability, sends the literal URL and removes the acknowledged PR', async () => {
  let finish!: (response: Response) => void;
  fetcher.mockImplementation((url: string) => url.endsWith('/merge') ? new Promise<Response>(r => { finish = r; }) :
    Promise.resolve(response(url.includes('/git/branch') ? { branch: 'feature/search' } : url.includes('/mergeability') ? detail : current)));
  render(nodePanel());
  fireEvent.click(await screen.findByRole('button', { name: 'Squash and merge' }));
  expect(fetcher.mock.calls.some(([url]) => url.endsWith('/merge'))).toBe(false);
  expect(screen.getByText('Squash-merge PR #42 and delete its branch on GitHub?')).toBeTruthy();
  expect(screen.getByRole('dialog', { name: 'Confirm squash merge' })).toBeTruthy();
  expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Cancel' }));
  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
  fireEvent.click(screen.getByRole('button', { name: 'Squash and merge' }));
  fireEvent.click(screen.getByRole('button', { name: 'Confirm squash merge' }));
  await waitFor(() => expect(finish).toBeTypeOf('function'));
  expect(fetcher).toHaveBeenCalledWith('/api/meshes/3/pulls/42/merge', expect.objectContaining({ method: 'POST', body: JSON.stringify({ url: pr.url }) }));
  expect((screen.getByRole('button', { name: 'Merging…' }) as HTMLButtonElement).disabled).toBe(true);
  await act(async () => finish(response({ url: pr.url, pr_number: 42 })));
  expect(await screen.findByText('PR #42 squash-merged. Branch deleted.')).toBeTruthy();
  expect(screen.queryByText('PR #42: Fix search')).toBeNull();
});

it('refuses a merge whose policy became blocked while confirming', async () => {
  render(nodePanel());
  fireEvent.click(await screen.findByRole('button', { name: 'Squash and merge' }));
  detail = { mergeable: true, mergeable_state: 'blocked' };
  fireEvent.click(screen.getByRole('button', { name: 'Confirm squash merge' }));
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', expect.stringContaining('no longer mergeable'));
  expect(fetcher.mock.calls.some(([url]) => url.endsWith('/merge'))).toBe(false);
});

it('reports an unacknowledged merge and requires refresh before retry', async () => {
  fetcher.mockImplementation((url: string) => Promise.resolve(response(
    url.includes('/git/branch') ? { branch: 'feature/search' } : url.includes('/mergeability') ? detail : url.endsWith('/merge') ? { error: 'Branch protection' } : current,
    url.endsWith('/merge') ? 500 : 200,
  )));
  render(nodePanel());
  fireEvent.click(await screen.findByRole('button', { name: 'Squash and merge' }));
  fireEvent.click(screen.getByRole('button', { name: 'Confirm squash merge' }));
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', expect.stringContaining('Branch protection'));
  expect(screen.queryByRole('button', { name: 'Squash and merge' })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Refresh PR status' }));
  expect(await screen.findByRole('button', { name: 'Squash and merge' })).toBeTruthy();
});

it.each([{}, { mergeable: true }, { mergeable_state: 'clean' }, { mergeable: 'true', mergeable_state: 'clean' }])('rejects malformed recheck %j without submitting a merge', async body => {
  fetcher.mockImplementation((url: string) => Promise.resolve(response(url.includes('/git/branch') ? { branch: 'feature/search' } : url.includes('/mergeability') ? body : current)));
  render(nodePanel());
  fireEvent.click(await screen.findByRole('button', { name: 'Squash and merge' }));
  fireEvent.click(screen.getByRole('button', { name: 'Confirm squash merge' }));
  expect(await screen.findByRole('alert')).toHaveProperty('textContent', expect.stringContaining('unexpected mergeability response'));
  expect(fetcher.mock.calls.some(([url]) => url.endsWith('/merge'))).toBe(false);
});

it('discloses incomplete mesh results without offering merge on unrelated PRs', async () => {
  current.completeness = { returned: 1, pages_fetched: 1, complete: false, incomplete_reason: 'safety_cap', reported_total: 1200 };
  render(<PullRequestsScreen mesh={{ id: 3, name: 'Repository' } as Mesh} onBack={vi.fn()} onAuthFailed={vi.fn()} />);
  expect(await screen.findByText(/Showing first 1 of 1200/)).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Squash and merge' })).toBeNull();
});

it('keeps the newer poll result when an earlier request resolves later', async () => {
  let old!: (response: Response) => void;
  fetcher.mockImplementationOnce(() => new Promise<Response>(r => { old = r; }));
  render(nodePanel());
  current = feed([{ ...pr, checks_state: 'pending', mergeable_state: 'blocked' }]);
  fireEvent.click(screen.getByRole('button', { name: 'Refresh PR status' }));
  expect(await screen.findByText('Checks: pending')).toBeTruthy();
  await act(async () => old(response(feed())));
  expect(screen.getByText('Checks: pending')).toBeTruthy();
});

it('ignores late auth failures after unmount and reports active authorization failures', async () => {
  const auth = vi.fn();
  let old!: (response: Response) => void;
  fetcher.mockImplementationOnce(() => new Promise<Response>(r => { old = r; }));
  const view = render(nodePanel(auth)); view.unmount();
  await act(async () => old(response({ error: 'Unauthorized' }, 403)));
  expect(auth).not.toHaveBeenCalled();
  fetcher.mockResolvedValue(response({ error: 'Unauthorized' }, 403));
  render(nodePanel(auth));
  await waitFor(() => expect(auth).toHaveBeenCalledTimes(1));
});
