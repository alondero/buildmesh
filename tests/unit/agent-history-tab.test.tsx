/**
 * Agent History probe tab — issue #2461.
 *
 * Four defects the tab shipped with, each pinned here:
 *   - the Lifecycle / Discovered sessions pair shared one static class
 *     string, so neither button showed which mode was selected;
 *   - Discovered sessions stacked a second Refresh control, because
 *     `ArchivedNodesTab` already renders its own `ProbeToolbar` (#813);
 *   - with no repositories the discovered-mode `<select>` rendered as a
 *     blank zero-option control above the empty state;
 *   - the card metadata line printed the stored value verbatim
 *     (`2026-10-17T22:43:58.211596700Z` — RFC 3339 at Rust nanosecond
 *     precision) inside a `break-all` container that split the trailing
 *     `Z` onto its own line.
 *
 * Assertions use plain DOM properties (no jest-dom matchers — the repo
 * has no jest-dom setup; see tests/setup/vitest.setup.ts).
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, render, screen, fireEvent, within } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { AgentHistoryTab } from '../../src/components/Probe/AgentHistoryTab';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import type { AgentNode } from '../../src/types/generated/AgentNode';

const MESH = { id: 1, name: 'api', path: '/repos/api', base_ref: 'main', use_worktree: true };

/**
 * An RFC 3339 string at the precision the backend actually stores: Rust
 * serialises `DateTime<Utc>` with nanosecond digits, so the last group
 * carries nine of them. `Date` parses this and truncates to milliseconds —
 * which is why the raw string, not its value, is what the UI showed.
 */
function nanosAgo(ms: number): string {
  const whole = new Date(Date.now() - ms).toISOString();
  return `${whole.slice(0, -1)}596700Z`;
}

function lifecycleAt(iso: string): NonNullable<AgentNode['lifecycle']> {
  return {
    session_id: 1, provider: null, kind: 'completion', status: 'archived', message: null,
    provider_event: null, provider_session_id: null, completion_reason: null, transcript_path: null,
    timestamp: iso, signal_health: 'ok', semantic_turn: null,
  };
}

function node(id: number, name: string, extra: Partial<AgentNode> = {}): AgentNode {
  return {
    id, mesh_id: MESH.id, name, path: MESH.path, branch: 'main', env: 'windows',
    provider: 'claude', use_worktree: false, status: 'archived', cli_session_id: 'saved-session',
    created_at: nanosAgo(3 * 60 * 60 * 1000),
    ...extra,
  } as AgentNode;
}

function mockBackend(history: AgentNode[] = [node(1, 'brave-fox')]) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'list_agent_history': return Promise.resolve(history);
      case 'discover_agent_nodes': return Promise.resolve([]);
      case 'list_providers': return Promise.resolve([]);
      default: return Promise.resolve({});
    }
  });
}

/** Both mode buttons, read through the group that names them. */
function modeButtons() {
  const group = screen.getByRole('group', { name: 'Agent history view' });
  return {
    lifecycle: within(group).getByRole('button', { name: 'Lifecycle' }),
    discovered: within(group).getByRole('button', { name: 'Discovered sessions' }),
  };
}

/** Class-token membership, not substring: `bg-bg-card` is a prefix of the
 *  inactive button's `hover:bg-bg-card-hover`. */
function hasClass(el: Element, token: string): boolean {
  return el.className.split(/\s+/).includes(token);
}

/**
 * Drain the reads the mounted tabs started before a test body returns.
 *
 * Rendering `AgentHistoryTab` fires `list_agent_history`; switching to
 * Discovered sessions mounts `ArchivedNodesTab`, which fires
 * `discover_agent_nodes` and `list_providers`. Every mock here resolves
 * immediately, so their `setHistory` / `setLoading` / `setSessions` /
 * `setProviderList` updates land in the microtask queue. A synchronous test
 * body returns before that queue drains, so the updates run outside React's
 * act boundary: React logs "not wrapped in act(...)" into stderr and the
 * orphaned render is still live when the next test mounts, which is the
 * cross-test leak the review flagged. Awaiting inside `act` keeps every one
 * of those updates inside the test that caused it.
 *
 * Which test the warning lands on is a race, so each test settles its own
 * reads rather than relying on the next one to absorb them.
 */
async function settleReads() {
  await act(async () => {});
}

describe('AgentHistoryTab (#2461)', () => {
  beforeEach(() => {
    useUIStore.setState({ probeOpen: true, probeTab: 'sessions', activeDiffFile: null });
    useMeshStore.setState({
      selectedMeshId: MESH.id,
      meshes: [MESH] as never,
      meshesById: new Map([[MESH.id, MESH]]) as never,
    });
    seedAgentNodes([]);
  });

  it('retains Host history when live agents change and the background reload fails', async () => {
    mockBackend();
    render(<AgentHistoryTab />);
    await screen.findByText('brave-fox');
    let reject!: (error: Error) => void;
    vi.mocked(invoke).mockImplementation(cmd => cmd === 'list_agent_history'
      ? new Promise((_, no) => { reject = no; }) : Promise.resolve([]));
    act(() => seedAgentNodes([node(2, 'new-live-agent', { status: 'running' })]));
    expect(screen.getByText('brave-fox')).toBeTruthy();
    await act(async () => reject(new Error('History offline')));
    expect((await screen.findByRole('alert')).textContent).toContain('History offline');
    expect(screen.getByText('brave-fox')).toBeTruthy();
    expect(screen.getByText('new-live-agent')).toBeTruthy();
  });

  it('gives the selected mode its own background and text colour, in a named group', async () => {
    mockBackend();
    render(<AgentHistoryTab />);

    const { lifecycle, discovered } = modeButtons();
    // Pre-fix both buttons carried one static string, so the selected mode
    // was invisible: neither had a background and both read as primary text.
    expect(lifecycle.getAttribute('aria-pressed')).toBe('true');
    expect(hasClass(lifecycle, 'bg-bg-card')).toBe(true);
    expect(hasClass(lifecycle, 'text-text-primary')).toBe(true);
    expect(discovered.getAttribute('aria-pressed')).toBe('false');
    expect(hasClass(discovered, 'bg-bg-card')).toBe(false);
    expect(hasClass(discovered, 'text-text-secondary')).toBe(true);

    fireEvent.click(discovered);

    const swapped = modeButtons();
    expect(swapped.discovered.getAttribute('aria-pressed')).toBe('true');
    expect(hasClass(swapped.discovered, 'bg-bg-card')).toBe(true);
    expect(swapped.lifecycle.getAttribute('aria-pressed')).toBe('false');
    expect(hasClass(swapped.lifecycle, 'bg-bg-card')).toBe(false);
    expect(hasClass(swapped.lifecycle, 'text-text-secondary')).toBe(true);

    // The click mounted `ArchivedNodesTab`; its own reads belong to this test.
    await screen.findByRole('button', { name: 'Refresh archived sessions' });
    await settleReads();
  });

  it('renders exactly one Refresh control in Discovered sessions mode', async () => {
    mockBackend();
    render(<AgentHistoryTab />);

    // Lifecycle owns this header's Refresh.
    expect(screen.getAllByRole('button', { name: 'Refresh agent history' })).toHaveLength(1);

    fireEvent.click(screen.getByRole('button', { name: 'Discovered sessions' }));

    // `ArchivedNodesTab` brings its own toolbar (#813); the header must not
    // add a second control ~50px above it.
    await screen.findByRole('button', { name: 'Refresh archived sessions' });
    const refreshes = screen.getAllByRole('button', { name: /^Refresh/ });
    expect(refreshes).toHaveLength(1);
    expect(refreshes[0].getAttribute('aria-label')).toBe('Refresh archived sessions');
    await settleReads();
  });

  it('never renders a zero-option repository select in Discovered sessions mode', async () => {
    useMeshStore.setState({ selectedMeshId: null, meshes: [] as never, meshesById: new Map() as never });
    mockBackend();
    render(<AgentHistoryTab />);

    // Lifecycle mode keeps the "All repositories" option, so its select has
    // something to show.
    const lifecycleSelect = screen.getByLabelText('History repository') as HTMLSelectElement;
    expect(lifecycleSelect.tagName).toBe('SELECT');
    expect(lifecycleSelect.options).toHaveLength(1);

    fireEvent.click(screen.getByRole('button', { name: 'Discovered sessions' }));

    // That option is discovered-mode-invisible, so the control would be a
    // blank zero-option select above the empty state.
    expect(await screen.findByText('Add a repository to discover previous sessions.')).toBeTruthy();
    expect(screen.queryByLabelText('History repository')).toBeNull();
    // No `ArchivedNodesTab` mounts without a repository, but this tab's own
    // history read is still in flight here.
    await settleReads();
  });

  it('keeps the repository select when discovered sessions have repositories to choose', async () => {
    mockBackend();
    render(<AgentHistoryTab />);

    fireEvent.click(screen.getByRole('button', { name: 'Discovered sessions' }));

    expect(await screen.findByLabelText('History repository')).toBeTruthy();
    const select = screen.getByLabelText('History repository') as HTMLSelectElement;
    expect(select.options).toHaveLength(1);
    expect(select.options[0].textContent).toBe('api');
    await screen.findByRole('button', { name: 'Refresh archived sessions' });
    await settleReads();
  });

  it('shows a readable relative timestamp instead of the stored nanosecond string', async () => {
    const observedAt = nanosAgo(5 * 60 * 1000);
    mockBackend([node(1, 'brave-fox', { lifecycle: lifecycleAt(observedAt) })]);
    render(<AgentHistoryTab />);

    const card = (await screen.findByText('brave-fox')).parentElement!;
    expect(card.textContent).toContain('Last observed 5m ago');
    // Nine fractional digits and a trailing `Z` are what `break-all` used to
    // push onto a line of their own.
    expect(card.textContent).not.toContain('596700');
    expect(card.textContent).not.toMatch(/\dZ\b/);
    await settleReads();
  });

  it('formats the created timestamp and wraps the metadata line on word boundaries', async () => {
    const createdAt = nanosAgo(3 * 60 * 60 * 1000);
    mockBackend([node(1, 'brave-fox', { created_at: createdAt })]);
    render(<AgentHistoryTab />);

    const name = await screen.findByText('brave-fox');
    const metadata = name.parentElement!.querySelector('p.break-words');
    expect(metadata).toBeTruthy();
    expect(metadata!.textContent).toBe('Created 3h ago');
    expect(metadata!.className).not.toContain('break-all');
    // The exact instant stays available without cluttering the panel.
    expect(metadata!.getAttribute('title')).toBe(new Date(createdAt).toLocaleString());
    await settleReads();
  });
});
