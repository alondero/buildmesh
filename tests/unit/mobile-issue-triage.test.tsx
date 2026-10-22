import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import IssuesScreen from '../../src/mobile/screens/IssuesScreen';
import type { GitHubIssueFeed } from '../../src/types/generated/GitHubIssueFeed';
import type { Mesh } from '../../src/mobile/api';

const mesh: Mesh = { id: 1, name: 'Repo', path: '/repo', created_at: '', scratchpad: '', sandbox: false };
const feed: GitHubIssueFeed = {
  items: [
    { number: 42, title: 'Fix deadline', body: 'Details', url: 'https://github.com/o/r/issues/42', state: 'open', labels: ['bug'], author: 'octo', blocked_by: [43, 99] },
    { number: 43, title: 'Prerequisite', body: '', url: '', state: 'open', labels: [], author: '', blocked_by: [] },
  ],
  completeness: { returned: 2, pages_fetched: 1, complete: false, incomplete_reason: 'safety_cap', reported_total: 120 },
};
let writes: { path: string; body: unknown }[];
let save: () => Promise<Response>;
let launch: () => Promise<Response>;
let catalog: () => Promise<Response>;
const response = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status });
beforeEach(() => {
  localStorage.clear();
  writes = [];
  save = async () => new Response(null, { status: 204 });
  launch = async () => response({ id: 9 });
  catalog = async () => response({ labels: ['bug', 'buildmesh:run'], autopilot_triggers: { 'buildmesh:run': ['Ship fixes'] } });
  vi.stubGlobal('fetch', vi.fn(async (path: string, options?: RequestInit) => {
    if (options?.method === 'POST') {
      writes.push({ path, body: JSON.parse(options.body as string) });
      if (path.endsWith('/label')) return save();
      return launch();
    }
    if (path.endsWith('/issues')) return response(feed);
    if (path.endsWith('/issue-labels')) return catalog();
    if (path === '/api/providers') return response([
      { id: 'claude', label: 'Claude Code', capabilities: { supports_prefill: true }, configuration: null },
      { id: 'terminal', label: 'Terminal', capabilities: { supports_prefill: false } },
      { id: 'missing', label: 'Missing', capabilities: { supports_prefill: true }, unavailable_reason: 'Not installed' },
      { id: 'saved', label: 'Saved recipe', capabilities: { supports_prefill: true }, configuration: { id: 'saved', name: 'Saved recipe', spawn_option_id: 'claude' } },
    ]);
    throw new Error(`Unexpected request: ${path}`);
  }));
});
afterEach(() => vi.unstubAllGlobals());

async function openIssue(onSpawned = vi.fn()) {
  render(<IssuesScreen mesh={mesh} visitId={1} onBack={() => {}} onSpawned={onSpawned} />);
  fireEvent.click(await screen.findByTestId('issue-42'));
  return onSpawned;
}

describe('phone issue triage', () => {
  it('keeps partial-feed counts visible even when a label filter has no matches', async () => {
    await openIssue();
    expect(screen.getByText(/Showing the first 2 of 120 issues/)).toBeTruthy();
    fireEvent.change(screen.getByTestId('issues-filter'), { target: { value: ' BUG ' } });
    expect(screen.getByTestId('issue-42')).toBeTruthy();
    expect(screen.queryByTestId('issue-43')).toBeNull();
    fireEvent.change(screen.getByTestId('issues-filter'), { target: { value: 'nothing' } });
    expect(screen.getByText('No matches.')).toBeTruthy();
    expect(screen.getByText(/Showing the first 2 of 120 issues/)).toBeTruthy();
  });

  it('shows the author and only blockers found in the unfiltered open feed', async () => {
    await openIssue();
    fireEvent.change(screen.getByTestId('issues-filter'), { target: { value: 'deadline' } });
    expect(screen.getByText('@octo')).toBeTruthy();
    expect(screen.getByText('Blocked by #43')).toBeTruthy();
    expect(screen.queryByText(/Blocked by.*99/)).toBeNull();
  });

  it('starts the mesh default from the split button without a confirmation step', async () => {
    const onSpawned = await openIssue();
    fireEvent.click(screen.getByTestId('issue-spawn-42'));
    await waitFor(() => expect(onSpawned).toHaveBeenCalledWith({ id: 9 }, { kind: 'issues', meshId: 1, visitId: 1 }));
    // The mesh default names neither `provider` nor `configuration_id`: the
    // backend resolves its own default chain.
    expect(writes).toEqual([{ path: '/api/meshes/1/issues/42/spawn', body: { title: 'Fix deadline' } }]);
  });

  it('offers the prompt-capable menu rows, saved configurations included', async () => {
    const onSpawned = await openIssue();
    fireEvent.click(screen.getByTestId('issue-spawn-42-menu'));
    const dialog = await screen.findByRole('dialog', { name: 'Choose a launch configuration' });
    await within(dialog).findByText('Claude Code');
    expect(within(dialog).queryByText('Terminal')).toBeNull();
    expect(within(dialog).queryByText('Missing')).toBeNull();
    expect(within(dialog).getByText('Saved recipe')).toBeTruthy();
    expect(writes).toEqual([]);
    fireEvent.click(within(dialog).getByText('Saved recipe'));
    await waitFor(() => expect(onSpawned).toHaveBeenCalledWith({ id: 9 }, { kind: 'issues', meshId: 1, visitId: 1 }));
    // A saved Launch Configuration launches its own Spawn Option with the
    // configuration's id beside it.
    expect(writes).toEqual([{ path: '/api/meshes/1/issues/42/spawn', body: { title: 'Fix deadline', provider: 'claude', configuration_id: 'saved' } }]);
  });

  it('reports a failed launch inside the open menu sheet', async () => {
    launch = async () => response({ error: 'spawn failed' }, 500);
    const onSpawned = await openIssue();
    fireEvent.click(screen.getByTestId('issue-spawn-42-menu'));
    const dialog = await screen.findByRole('dialog');
    await within(dialog).findByText('Claude Code');
    fireEvent.click(within(dialog).getByText('Claude Code'));
    // The sheet stays open so the user can pick again, so the failure has to
    // be reported inside it — behind the backdrop the screen's own error
    // line would be unreadable.
    await waitFor(() => expect(within(dialog).getByText('spawn failed')).toBeTruthy());
    expect(onSpawned).not.toHaveBeenCalled();
    expect(screen.getByRole('dialog')).toBeTruthy();
  });

  it('closes the launch menu without creating a node', async () => {
    await openIssue();
    fireEvent.click(screen.getByTestId('issue-spawn-42-menu'));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('Claude Code')).toBeTruthy();
    fireEvent.click(screen.getByTestId('issue-launch-menu-backdrop'));
    expect(writes).toEqual([]);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('adds and removes one tag after acknowledgement, identifies Autopilot and preserves other tags', async () => {
    await openIssue();
    fireEvent.click(screen.getByRole('button', { name: 'Edit tags for issue #42' }));
    const checkbox = await screen.findByRole('checkbox', { name: /buildmesh:run/ });
    expect(screen.getByText(/Ship fixes/)).toBeTruthy();
    let resolve!: (response: Response) => void;
    save = () => new Promise(r => { resolve = r; });
    fireEvent.click(checkbox);
    expect((checkbox as HTMLInputElement).checked).toBe(false);
    expect((checkbox as HTMLInputElement).disabled).toBe(true);
    resolve(new Response(null, { status: 204 }));
    await waitFor(() => expect((checkbox as HTMLInputElement).checked).toBe(true));
    expect((screen.getByRole('checkbox', { name: 'bug' }) as HTMLInputElement).checked).toBe(true);
    save = async () => new Response(null, { status: 204 });
    fireEvent.click(checkbox);
    await waitFor(() => expect((checkbox as HTMLInputElement).checked).toBe(false));
    expect(writes.map(w => w.body)).toEqual([{ label: 'buildmesh:run', present: true }, { label: 'buildmesh:run', present: false }]);
  });

  it('retains tags and permits retry when GitHub rejects a write', async () => {
    save = async () => response({ error: 'GitHub permission denied' }, 500);
    await openIssue();
    fireEvent.click(screen.getByRole('button', { name: 'Edit tags for issue #42' }));
    const checkbox = await screen.findByRole('checkbox', { name: /buildmesh:run/ });
    fireEvent.click(checkbox);
    expect(await screen.findByRole('alert')).toHaveProperty('textContent', 'GitHub permission denied');
    expect((checkbox as HTMLInputElement).checked).toBe(false);
    save = async () => new Response(null, { status: 204 });
    fireEvent.click(checkbox);
    await waitFor(() => expect((checkbox as HTMLInputElement).checked).toBe(true));
  });

  it('keeps the launch menu open during a launch and consumes back and backdrop', async () => {
    let resolve!: (response: Response) => void;
    launch = () => new Promise(r => { resolve = r; });
    const backHandlerRef = { current: null as (() => boolean) | null };
    const onSpawned = vi.fn();
    const onBack = vi.fn();
    render(<IssuesScreen mesh={mesh} visitId={1} onBack={onBack} onSpawned={onSpawned} backHandlerRef={backHandlerRef} />);
    fireEvent.click(await screen.findByTestId('issue-42'));
    fireEvent.click(screen.getByTestId('issue-spawn-42-menu'));
    const dialog = await screen.findByRole('dialog');
    fireEvent.click(within(dialog).getByText('Claude Code'));
    await waitFor(() => expect(writes).toHaveLength(1));
    expect(backHandlerRef.current?.()).toBe(true);
    fireEvent.click(screen.getByTestId('issue-launch-menu-backdrop'));
    fireEvent.click(screen.getByRole('button', { name: 'Back' }));
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(onBack).not.toHaveBeenCalled();
    expect(writes).toEqual([{ path: '/api/meshes/1/issues/42/spawn', body: { title: 'Fix deadline', provider: 'claude' } }]);
    resolve(response({ id: 9 }));
    await waitFor(() => expect(onSpawned).toHaveBeenCalledOnce());
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('retries a repository tag load without treating it as an empty catalog', async () => {
    catalog = async () => response({ error: 'Label read failed' }, 500);
    await openIssue();
    fireEvent.click(screen.getByRole('button', { name: 'Edit tags for issue #42' }));
    expect(await screen.findByText('Label read failed')).toBeTruthy();
    expect(screen.queryByText('No repository labels')).toBeNull();
    catalog = async () => response({ labels: ['bug'], autopilot_triggers: {} });
    fireEvent.click(screen.getByRole('button', { name: 'Retry loading tags' }));
    expect(await screen.findByRole('checkbox', { name: 'bug' })).toBeTruthy();
  });

  it('reconciles an acknowledged tag after filtering its row out and back in', async () => {
    let resolve!: (response: Response) => void;
    save = () => new Promise(r => { resolve = r; });
    await openIssue();
    fireEvent.click(screen.getByRole('button', { name: 'Edit tags for issue #42' }));
    fireEvent.click(await screen.findByRole('checkbox', { name: /buildmesh:run/ }));
    fireEvent.change(screen.getByTestId('issues-filter'), { target: { value: 'prerequisite' } });
    resolve(new Response(null, { status: 204 }));
    await waitFor(() => expect(writes).toHaveLength(1));
    fireEvent.change(screen.getByTestId('issues-filter'), { target: { value: 'buildmesh:run' } });
    expect(await screen.findByTestId('issue-42')).toBeTruthy();
  });

  it('rejects a late tag completion belonging to a different mesh', async () => {
    let resolve!: (response: Response) => void;
    save = () => new Promise(r => { resolve = r; });
    const view = render(<IssuesScreen mesh={mesh} visitId={1} onBack={() => {}} onSpawned={() => {}} />);
    await screen.findByTestId('issue-42');
    fireEvent.click(screen.getByRole('button', { name: 'Edit tags for issue #42' }));
    fireEvent.click(await screen.findByRole('checkbox', { name: /buildmesh:run/ }));
    view.rerender(<IssuesScreen mesh={{ ...mesh, id: 2 }} visitId={2} onBack={() => {}} onSpawned={() => {}} />);
    await screen.findByTestId('issue-42');
    resolve(new Response(null, { status: 204 }));
    fireEvent.click(screen.getByRole('button', { name: 'Edit tags for issue #42' }));
    const checkbox = await screen.findByRole('checkbox', { name: /buildmesh:run/ });
    await waitFor(() => expect((checkbox as HTMLInputElement).disabled).toBe(false));
    expect((checkbox as HTMLInputElement).checked).toBe(false);
  });

  it('routes an authorization failure from a tag write to pairing', async () => {
    save = async () => response({ error: 'Device revoked' }, 401);
    const onAuthFailed = vi.fn();
    render(<IssuesScreen mesh={mesh} visitId={1} onBack={() => {}} onSpawned={() => {}} onAuthFailed={onAuthFailed} />);
    await screen.findByTestId('issue-42');
    fireEvent.click(screen.getByRole('button', { name: 'Edit tags for issue #42' }));
    fireEvent.click(await screen.findByRole('checkbox', { name: /buildmesh:run/ }));
    await waitFor(() => expect(onAuthFailed).toHaveBeenCalledOnce());
  });
});
