import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { GitIssuesTab } from '../../src/components/Probe/GitIssuesTab';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import type { AutopilotCircuit } from '../../src/types/generated/AutopilotCircuit';
import type { GitHubIssue } from '../../src/types/generated/GitHubIssue';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const issue = { number: 101, title: 'Implement the widget', body: 'Widget details', url: '', state: 'open', labels: ['bug'], blocked_by: [], author: '' };
const mesh = { id: 42, name: 'demo', path: '/repos/demo', layout: 'single', position: 0, created_at: '', scratchpad: '', sandbox: false };
function circuit(label: string, enabled = true, meshId = 42, type = 'github_issue_label'): AutopilotCircuit {
  return { id: 1, mesh_id: meshId, name: 'Implementation', description: '', enabled, concurrency_limit: 1, is_preset: false, created_at: '', updated_at: '',
    graph_json: JSON.stringify({ version: 3, nodes: [{ id: 'trigger', type: { type, label } }], edges: [] }) };
}
function backend(circuits: AutopilotCircuit[] = []) {
  vi.mocked(invoke).mockImplementation(async (cmd, args) => {
    if (cmd === 'get_repo_issues') return [{ ...issue, labels: [...issue.labels] }];
    if (cmd === 'list_circuits') return circuits;
    if (cmd === 'get_repo_labels') return ['bug', 'ready-for-agent', 'team/ui'];
    if (cmd === 'set_issue_label') {
      const { label, present } = args as { label: string; present: boolean };
      issue.labels = present ? [...issue.labels, label] : issue.labels.filter(value => value !== label);
    }
    if (cmd === 'list_providers') return [];
    return null;
  });
}

describe('GitHub issue tags', () => {
  beforeEach(() => {
    issue.labels = ['bug'];
    useMeshStore.setState({ meshesById: new Map([[42, mesh]]), selectedMeshId: 42 });
    useUIStore.setState({ probeOpen: true, probeTab: 'issues', activeDiffFile: null });
  });

  it('edits repository labels without expanding the issue and acknowledges add/remove on the card', async () => {
    backend([circuit('ready-for-agent')]);
    render(<GitIssuesTab />);
    const tags = await screen.findByRole('button', { name: 'Edit tags for issue #101' });
    tags.focus();
    await userEvent.keyboard('{Enter}');
    const ready = await screen.findByRole('checkbox', { name: /ready-for-agent/ });
    await userEvent.click(ready);
    await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith('set_issue_label', { meshId: 42, issueNumber: 101, label: 'ready-for-agent', present: true }));
    const row = document.querySelector('[data-issue-row="101"]')!;
    await waitFor(() => expect(row.querySelector('[data-circuit-trigger-label="ready-for-agent"]')).not.toBeNull());
    expect(row.querySelector('[data-issue-body-expanded]')).toBeNull();
    await userEvent.click(within(row as HTMLElement).getByRole('checkbox', { name: /^bug$/ }));
    await waitFor(() => expect(issue.labels).toEqual(['ready-for-agent']));
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('checkbox')).toBeNull();
    expect(document.activeElement).toBe(tags);
  });

  it('highlights customized issue trigger labels ahead of overflow only on the enabled current mesh', async () => {
    issue.labels = ['bug', 'other', 'third', 'team/ui', 'ready-for-agent'];
    backend([circuit('team/ui'), circuit('ready-for-agent', false), circuit('bug', true, 99), circuit('other', true, 42, 'github_pull_request_label')]);
    render(<GitIssuesTab />);
    await waitFor(() => expect(document.querySelector('[data-circuit-trigger-label="team/ui"]')).not.toBeNull());
    expect(document.querySelectorAll('[data-circuit-trigger-label]')).toHaveLength(1);
    expect(screen.getByTitle(/Watched by enabled Autopilot Circuit: Implementation/)).toBeTruthy();
  });

  it('keeps healthy Circuit highlights when another graph is malformed', async () => {
    issue.labels = ['ready-for-agent'];
    backend([{ ...circuit('broken'), graph_json: 'not json' }, circuit('ready-for-agent')]);
    render(<GitIssuesTab />);
    await waitFor(() => expect(document.querySelector('[data-circuit-trigger-label="ready-for-agent"]')).not.toBeNull());
    expect(screen.queryByText(/Autopilot label status unavailable/)).toBeNull();
  });

  it('shares one repository label load across issue editors', async () => {
    backend();
    const original = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation((cmd, args, opts) => cmd === 'get_repo_issues'
      ? Promise.resolve([{ ...issue }, { ...issue, number: 102, title: 'Second issue' }])
      : original(cmd, args, opts));
    render(<GitIssuesTab />);
    const first = await screen.findByRole('button', { name: 'Edit tags for issue #101' });
    const second = screen.getByRole('button', { name: 'Edit tags for issue #102' });
    await waitFor(() => expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'get_repo_labels')).toHaveLength(1));
    await userEvent.click(first);
    await screen.findByRole('checkbox', { name: 'team/ui' });
    await userEvent.click(first);
    await userEvent.click(second);
    await screen.findByRole('checkbox', { name: 'team/ui' });
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'get_repo_labels')).toHaveLength(1);
  });

  it('keeps an acknowledged edit when an older GitHub search read returns stale labels', async () => {
    backend();
    const original = vi.mocked(invoke).getMockImplementation()!;
    const staleSearch = deferred<GitHubIssue[]>();
    let issueReads = 0;
    vi.mocked(invoke).mockImplementation((cmd, args, opts) => {
      if (cmd === 'get_repo_issues') {
        issueReads += 1;
        return issueReads === 1 ? Promise.resolve([{ ...issue, labels: ['bug'] }]) : staleSearch.promise;
      }
      return original(cmd, args, opts);
    });
    render(<GitIssuesTab />);
    await screen.findByRole('button', { name: 'Edit tags for issue #101' });
    await userEvent.click(screen.getByRole('button', { name: 'Refresh issues' }));
    await waitFor(() => expect(issueReads).toBe(2));
    await userEvent.click(screen.getByRole('button', { name: 'Edit tags for issue #101' }));
    await screen.findByRole('checkbox', { name: 'ready-for-agent' });
    await userEvent.click(screen.getByRole('checkbox', { name: 'ready-for-agent' }));
    await waitFor(() => expect(document.querySelector('[data-issue-label="ready-for-agent"]')).not.toBeNull());
    await act(async () => staleSearch.resolve([{ ...issue, labels: ['bug'] }]));
    expect(document.querySelector('[data-issue-label="ready-for-agent"]')).not.toBeNull();
    expect(issueReads).toBe(2);
  });

  it('keeps labels unchanged on failure and allows retry', async () => {
    backend();
    const original = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation((cmd, args, opts) => cmd === 'set_issue_label' ? Promise.reject(new Error('GitHub denied permission')) : original(cmd, args, opts));
    render(<GitIssuesTab />);
    await userEvent.click(await screen.findByRole('button', { name: 'Edit tags for issue #101' }));
    await userEvent.click(await screen.findByRole('checkbox', { name: 'ready-for-agent' }));
    expect((await screen.findByRole('alert')).textContent).toContain('GitHub denied permission');
    expect(issue.labels).toEqual(['bug']);
    expect((screen.getByRole('checkbox', { name: 'ready-for-agent' }) as HTMLInputElement).checked).toBe(false);
    backend();
    await userEvent.click(screen.getByRole('checkbox', { name: 'ready-for-agent' }));
    await waitFor(() => expect(issue.labels).toContain('ready-for-agent'));
  });

  it('disables writes while pending and drops completion after a mesh switch', async () => {
    backend();
    const original = vi.mocked(invoke).getMockImplementation()!;
    let finish!: () => void;
    vi.mocked(invoke).mockImplementation((cmd, args, opts) => cmd === 'set_issue_label' ? new Promise<void>(resolve => { finish = resolve; }) : original(cmd, args, opts));
    render(<GitIssuesTab />);
    await userEvent.click(await screen.findByRole('button', { name: 'Edit tags for issue #101' }));
    await userEvent.click(await screen.findByRole('checkbox', { name: 'ready-for-agent' }));
    expect((screen.getByRole('checkbox', { name: 'bug' }) as HTMLInputElement).disabled).toBe(true);
    act(() => useMeshStore.setState({ selectedMeshId: 99, meshesById: new Map([[99, { ...mesh, id: 99 }]]) }));
    await screen.findByRole('button', { name: 'Edit tags for issue #101' });
    await act(async () => finish());
    expect(document.querySelector('[data-issue-label="ready-for-agent"]')).toBeNull();
  });

  it('reports a label-load failure and retries; search and outside click work', async () => {
    backend();
    const original = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation((cmd, args, opts) => cmd === 'get_repo_labels' ? Promise.reject(new Error('Offline')) : original(cmd, args, opts));
    render(<GitIssuesTab />);
    await userEvent.click(await screen.findByRole('button', { name: 'Edit tags for issue #101' }));
    expect((await screen.findByRole('alert')).textContent).toContain('Offline');
    backend();
    await userEvent.click(screen.getByRole('button', { name: 'Retry loading tags' }));
    await screen.findByRole('checkbox', { name: 'team/ui' });
    await userEvent.type(screen.getByRole('textbox', { name: 'Filter tags' }), 'team/');
    expect(screen.queryByRole('checkbox', { name: 'bug' })).toBeNull();
    fireEvent.mouseDown(document.body);
    expect(screen.queryByRole('textbox', { name: 'Filter tags' })).toBeNull();
  });

  it('reconciles an acknowledged write while the issue is filtered out', async () => {
    backend([circuit('ready-for-agent')]);
    const original = vi.mocked(invoke).getMockImplementation()!;
    const write = deferred<void>();
    vi.mocked(invoke).mockImplementation((cmd, args, opts) => cmd === 'set_issue_label' ? write.promise : original(cmd, args, opts));
    render(<GitIssuesTab />);
    await userEvent.click(await screen.findByRole('button', { name: 'Edit tags for issue #101' }));
    await userEvent.click(await screen.findByRole('checkbox', { name: /ready-for-agent/ }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Filter issues' }), 'hidden');
    expect(screen.queryByRole('button', { name: 'Edit tags for issue #101' })).toBeNull();
    issue.labels.push('ready-for-agent');
    await act(async () => write.resolve());
    await userEvent.clear(screen.getByRole('textbox', { name: 'Filter issues' }));
    await waitFor(() => expect(document.querySelector('[data-issue-label="ready-for-agent"]')).not.toBeNull());
  });

  it('preserves description disclosure across tag edits and keeps a rejection visible after closing the editor', async () => {
    backend();
    render(<GitIssuesTab />);
    await userEvent.click(await screen.findByRole('button', { name: 'Expand issue #101 description' }));
    await userEvent.click(screen.getByRole('button', { name: 'Edit tags for issue #101' }));
    await userEvent.click(await screen.findByRole('checkbox', { name: 'ready-for-agent' }));
    await waitFor(() => expect((screen.getByRole('checkbox', { name: 'ready-for-agent' }) as HTMLInputElement).disabled).toBe(false));
    expect(document.querySelector('[data-issue-body-expanded]')).not.toBeNull();
    const original = vi.mocked(invoke).getMockImplementation()!;
    const write = deferred<void>();
    vi.mocked(invoke).mockImplementation((cmd, args, opts) => cmd === 'set_issue_label' ? write.promise : original(cmd, args, opts));
    await userEvent.click(screen.getByRole('checkbox', { name: 'bug' }));
    await userEvent.keyboard('{Escape}');
    await act(async () => write.reject(new Error('Write failed')));
    expect(screen.getByRole('alert').textContent).toContain('Write failed');
    expect(screen.queryByRole('checkbox')).toBeNull();
    expect(issue.labels).toContain('bug');
  });

  it.each(['old-first', 'new-first'])('fences issue and circuit reads across mesh switches (%s)', async order => {
    backend();
    const original = vi.mocked(invoke).getMockImplementation()!;
    const oldIssues = deferred<GitHubIssue[]>();
    const newIssues = deferred<GitHubIssue[]>();
    const oldCircuits = deferred<AutopilotCircuit[]>();
    const newCircuits = deferred<AutopilotCircuit[]>();
    const oldLabels = deferred<string[]>();
    const newLabels = deferred<string[]>();
    vi.mocked(invoke).mockImplementation((cmd, args, opts) => {
      const meshId = (args as { meshId?: number } | undefined)?.meshId;
      if (cmd === 'get_repo_issues') return meshId === 42 ? oldIssues.promise : newIssues.promise;
      if (cmd === 'list_circuits') return meshId === 42 ? oldCircuits.promise : newCircuits.promise;
      if (cmd === 'get_repo_labels') return meshId === 42 ? oldLabels.promise : newLabels.promise;
      return original(cmd, args, opts);
    });
    render(<GitIssuesTab />);
    act(() => useMeshStore.setState({ selectedMeshId: 99, meshesById: new Map([[99, { ...mesh, id: 99 }]]) }));
    const old = async () => { oldIssues.resolve([{ ...issue, labels: ['bug'] }]); oldCircuits.resolve([circuit('bug')]); oldLabels.resolve(['old-only']); };
    const fresh = async () => { newIssues.resolve([{ ...issue, title: 'New mesh issue', labels: ['team/ui'] }]); newCircuits.resolve([circuit('team/ui', true, 99)]); newLabels.resolve(['new-only']); };
    await act(order === 'old-first' ? old : fresh);
    await act(order === 'old-first' ? fresh : old);
    expect(await screen.findByText('New mesh issue')).toBeTruthy();
    expect(document.querySelector('[data-circuit-trigger-label="team/ui"]')).not.toBeNull();
    expect(document.querySelector('[data-issue-label="bug"]')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Edit tags for issue #101' }));
    expect(await screen.findByRole('checkbox', { name: 'new-only' })).toBeTruthy();
    expect(screen.queryByRole('checkbox', { name: 'old-only' })).toBeNull();
  });

  it('removes highlights when the circuit is disabled and reports status lookup errors', async () => {
    issue.labels = ['ready-for-agent'];
    backend([circuit('ready-for-agent')]);
    render(<GitIssuesTab />);
    await waitFor(() => expect(document.querySelector('[data-circuit-trigger-label]')).not.toBeNull());
    backend([circuit('ready-for-agent', false)]);
    await userEvent.click(screen.getByRole('button', { name: 'Refresh issues' }));
    await waitFor(() => expect(document.querySelector('[data-circuit-trigger-label]')).toBeNull());
    const original = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation((cmd, args, opts) => cmd === 'list_circuits' ? Promise.reject(new Error('Circuit unavailable')) : original(cmd, args, opts));
    await userEvent.click(screen.getByRole('button', { name: 'Refresh issues' }));
    expect((await screen.findByRole('alert')).textContent).toContain('Circuit unavailable');
    expect(document.querySelector('[data-circuit-trigger-label]')).toBeNull();
  });
});
