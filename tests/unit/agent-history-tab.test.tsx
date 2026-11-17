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
import { act, render, screen, fireEvent, within, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { AgentHistoryTab, sortHistoryEntries } from '../../src/components/Probe/AgentHistoryTab';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { AgentHistoryEntry } from '../../src/types/generated/AgentHistoryEntry';

const MESH = { id: 1, name: 'api', path: '/repos/api', base_ref: 'main', use_worktree: true };
const OTHER_MESH = { id: 2, name: 'web', path: '/repos/web', base_ref: 'main', use_worktree: true };

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

/**
 * A history row: the node plus the two timestamps `list_agent_history` now
 * carries. The wire shape is flat (ts-rs `flatten`), so this is just an
 * `AgentNode` with two extra nullable fields. Defaults are null so a fixture
 * that does not care about timing keeps rendering the `Created …` clause.
 */
function entry(
  id: number,
  name: string,
  extra: Partial<AgentNode> = {},
  timing: { status_changed_at?: string | null; session_started_at?: number | null } = {},
): AgentHistoryEntry {
  return {
    ...node(id, name, extra),
    status_changed_at: timing.status_changed_at ?? null,
    session_started_at: timing.session_started_at ?? null,
  };
}

function mockBackend(history: AgentHistoryEntry[] = [entry(1, 'brave-fox')]) {
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
    mockBackend([entry(1, 'brave-fox', { lifecycle: lifecycleAt(observedAt) }, { status_changed_at: observedAt })]);
    render(<AgentHistoryTab />);

    const card = (await screen.findByText('brave-fox')).parentElement!;
    expect(card.textContent).toContain('Archived 5m ago');
    // Nine fractional digits and a trailing `Z` are what `break-all` used to
    // push onto a line of their own.
    expect(card.textContent).not.toContain('596700');
    expect(card.textContent).not.toMatch(/\dZ\b/);
    await settleReads();
  });

  it('shows when a session last changed and when its process started', async () => {
    const changedAt = nanosAgo(2 * 60 * 60 * 1000);
    const startedAt = Date.now() - 4 * 60 * 60 * 1000;
    mockBackend([entry(1, 'brave-fox', {}, { status_changed_at: changedAt, session_started_at: startedAt })]);
    render(<AgentHistoryTab />);

    const card = (await screen.findByText('brave-fox')).parentElement!;
    expect(card.textContent).toContain('Archived 2h ago');
    expect(card.textContent).toContain('Started 4h ago');
    await settleReads();
  });

  it('formats the created timestamp and wraps the metadata line on word boundaries', async () => {
    const createdAt = nanosAgo(3 * 60 * 60 * 1000);
    mockBackend([entry(1, 'brave-fox', { created_at: createdAt })]);
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

  describe('meaningful recency ordering (#2801)', () => {
    /** The row's timing line: "Last changed …", "Archived …" or "Created …". */
    function timingLine(name: string): HTMLElement {
      return screen.getByText(name).parentElement!.querySelector('p.break-words') as HTMLElement;
    }

    /** DOM order, read the way a keyboard or pointer user would experience it. */
    function appearsBefore(first: string, second: string): boolean {
      const a = screen.getByText(first);
      const b = screen.getByText(second);
      return Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
    }

    function row(id: number, created: string, changed: string | null = null): AgentHistoryEntry {
      return entry(id, `agent-${id}`, { created_at: created }, { status_changed_at: changed });
    }

    it('ranks an older session that changed today above a newer inactive one', async () => {
      mockBackend([
        entry(1, 'long-lived-agent', { created_at: '2026-01-05T09:00:00Z' }, { status_changed_at: '2026-11-16T18:00:00Z' }),
        entry(2, 'one-off-agent', { created_at: '2026-11-15T09:00:00Z' }),
      ]);
      render(<AgentHistoryTab />);
      await screen.findByText('long-lived-agent');

      // Creation date used to own the list, so the newer one-off session sat
      // above the long-lived one the user had just touched.
      expect(appearsBefore('long-lived-agent', 'one-off-agent')).toBe(true);
      // The visible labels agree with the ordering: the older row says when it
      // moved, the newer one falls back to when it was created.
      expect(timingLine('long-lived-agent').textContent).toMatch(/^Archived /);
      expect(timingLine('one-off-agent').textContent).toMatch(/^Created /);

      fireEvent.change(screen.getByLabelText('History ordering'), { target: { value: 'created' } });
      expect(appearsBefore('one-off-agent', 'long-lived-agent')).toBe(true);
      await settleReads();
    });

    it('keeps the chosen ordering when search, repository and status filters change', async () => {
      useMeshStore.setState({
        meshes: [MESH, OTHER_MESH] as never,
        meshesById: new Map([[MESH.id, MESH], [OTHER_MESH.id, OTHER_MESH]]) as never,
      });
      mockBackend([
        entry(1, 'alpha-agent', { created_at: '2026-01-05T09:00:00Z' }, { status_changed_at: '2026-11-16T18:00:00Z' }),
        entry(2, 'beta-agent', { mesh_id: OTHER_MESH.id, path: OTHER_MESH.path, created_at: '2026-11-15T09:00:00Z' }),
        entry(3, 'gamma-chief', { created_at: '2026-11-10T09:00:00Z' }, { status_changed_at: '2026-11-16T20:00:00Z' }),
      ]);
      render(<AgentHistoryTab />);
      await screen.findByText('alpha-agent');

      fireEvent.change(screen.getByLabelText('History ordering'), { target: { value: 'created' } });
      expect(appearsBefore('beta-agent', 'alpha-agent')).toBe(true);

      fireEvent.change(screen.getByLabelText('Search agent history'), { target: { value: 'agent' } });
      expect(screen.queryByText('gamma-chief')).toBeNull();
      expect(appearsBefore('beta-agent', 'alpha-agent')).toBe(true);

      fireEvent.change(screen.getByLabelText('History status'), { target: { value: 'archived' } });
      expect(appearsBefore('beta-agent', 'alpha-agent')).toBe(true);

      // A repository filter changes the set, not the selected ordering.
      fireEvent.change(screen.getByLabelText('Search agent history'), { target: { value: '' } });
      fireEvent.change(screen.getByLabelText('History repository'), { target: { value: String(MESH.id) } });
      expect(screen.queryByText('beta-agent')).toBeNull();
      expect(appearsBefore('gamma-chief', 'alpha-agent')).toBe(true);
      await settleReads();
    });

    it('reorders on Refresh but not on live terminal activity', async () => {
      const old = entry(1, 'long-lived-agent', { created_at: '2026-01-05T09:00:00Z' }, { status_changed_at: '2026-11-16T18:00:00Z' });
      const inactive = entry(2, 'one-off-agent', { created_at: '2026-11-15T09:00:00Z' });
      let rows = [old, inactive];
      vi.mocked(invoke).mockImplementation((cmd: string) => cmd === 'list_agent_history'
        ? Promise.resolve(rows) : Promise.resolve([]));
      const liveRow = (status: AgentNode['status']) => node(2, 'one-off-agent', { status, created_at: inactive.created_at });
      seedAgentNodes([liveRow('running')]);
      render(<AgentHistoryTab />);
      await screen.findByText('long-lived-agent');
      expect(appearsBefore('long-lived-agent', 'one-off-agent')).toBe(true);

      // Terminal output changes the live row's status repeatedly. The order is
      // derived from durable history timing, so the row must not move and the
      // list must not be read again.
      act(() => seedAgentNodes([liveRow('awaiting_input')]));
      expect(appearsBefore('long-lived-agent', 'one-off-agent')).toBe(true);
      const historyReads = () => vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'list_agent_history');
      expect(historyReads()).toHaveLength(1);

      // Refresh acknowledges the newer change, and only then may the order move.
      rows = [old, { ...inactive, status_changed_at: '2026-11-17T08:00:00Z' }];
      fireEvent.click(screen.getByRole('button', { name: 'Refresh agent history' }));
      await waitFor(() => expect(appearsBefore('one-off-agent', 'long-lived-agent')).toBe(true));
      expect(historyReads()).toHaveLength(2);
    });

    it('defaults to last changed with creation fallback and offers Created explicitly', () => {
      const rows = [row(2, '2026-11-15T09:00:00Z'), row(1, '2026-01-05T09:00:00Z', '2026-11-16T18:00:00Z')];
      expect(sortHistoryEntries(rows, 'changed').map(r => r.id)).toEqual([1, 2]);
      expect(sortHistoryEntries(rows, 'created').map(r => r.id)).toEqual([2, 1]);
    });

    it('compares instants across UTC offsets and legacy zoneless text', () => {
      const rows = [
        row(1, '2026-01-01T00:00:00Z', '2026-11-16T11:00:00Z'),
        row(2, '2026-01-01T00:00:00Z', '2026-11-16T09:30:00-05:00'), // 14:30Z
        row(3, '2026-01-01T00:00:00Z', '2026-11-16 10:05:00'), // SQLite writes UTC
        row(4, '2026-01-01T00:00:00Z', '2026-11-16T10:06:00Z'),
      ];
      // 2 (14:30Z) > 1 (11:00Z) > 4 (10:06Z) > 3 (10:05Z). Comparing the raw
      // strings would put 2's `09:30` below the `10:`/`11:` values.
      expect(sortHistoryEntries(rows, 'changed').map(r => r.id)).toEqual([2, 1, 4, 3]);
    });

    it('keeps null and unreadable timestamps at the bottom', () => {
      const rows = [
        row(1, '2020-01-01T00:00:00Z', 'not-a-timestamp'),
        row(2, '2023-01-01T00:00:00Z'),
        row(3, '2021-01-01T00:00:00Z', '2026-11-16T00:00:00Z'),
        row(4, 'garbage'),
      ];
      // An unreadable `status_changed_at` falls back to creation; an unreadable
      // creation time sorts last instead of being treated as "now".
      expect(sortHistoryEntries(rows, 'changed').map(r => r.id)).toEqual([3, 2, 1, 4]);
      expect(sortHistoryEntries(rows, 'created').map(r => r.id)).toEqual([2, 3, 1, 4]);
    });

    it('breaks ties deterministically by creation time then id', () => {
      const changed = '2026-11-16T00:00:00Z';
      const rows = [
        row(1, '2026-01-01T00:00:00Z', changed),
        row(3, '2026-02-01T00:00:00Z', changed),
        row(2, '2026-02-01T00:00:00Z', changed),
      ];
      expect(sortHistoryEntries(rows, 'changed').map(r => r.id)).toEqual([3, 2, 1]);
    });
  });
});
