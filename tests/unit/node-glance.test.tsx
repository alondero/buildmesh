import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import type { NodeGlance } from '../../src/types/generated/NodeGlance';
import {
  activityHeadline, formatElapsed, glanceTimings, timingLine, toolCountLine, unavailableCopy,
} from '../../src/lib/nodeGlance';

const getNodeGlance = vi.fn<(nodeId: number) => Promise<NodeGlance>>();
vi.mock('../../src/lib/tauri/agentNode', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../src/lib/tauri/agentNode')>()),
  getNodeGlance: (nodeId: number) => getNodeGlance(nodeId),
}));

import { NodeItem } from '../../src/components/Sidebar/NodeItem';
import { NodeGlancePanel } from '../../src/mobile/components/NodeGlancePanel';
import { getMeshColor } from '../../src/lib/meshColors';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';

// The same sample the Rust wire gate pins for the phone
// (android/app/src/test/resources/mobile-wire/node_glance.json).
const running: NodeGlance = {
  node_id: 7,
  status: 'running',
  observed_at_ms: 1_793_793_600_000,
  status_since_ms: 1_793_792_880_000,
  session_started_at_ms: 1_793_786_400_000,
  created_at_ms: 1_793_786_000_000,
  transcript: {
    last_prompt: 'Fix the login redirect',
    last_message: 'The redirect drops the query string; fixing it now.',
    message_after_prompt: true,
    activity: [
      { tool: 'Edit', target: 'src/auth/redirect.ts' },
      { tool: 'Bash', target: 'npm test' },
    ],
    tool_calls_since_prompt: 14,
    updated_at_ms: 1_793_793_592_000,
  },
  transcript_unavailable: null,
};

const unsupported: NodeGlance = {
  ...running,
  node_id: 8,
  status: 'ready',
  status_since_ms: 1_793_793_300_000,
  session_started_at_ms: null,
  transcript: null,
  transcript_unavailable: 'unsupported',
};

describe('Node Glance presentation', () => {
  it('formats elapsed time compactly', () => {
    expect(formatElapsed(-5)).toBe('0s');
    expect(formatElapsed(45_000)).toBe('45s');
    expect(formatElapsed(12 * 60_000)).toBe('12m');
    expect(formatElapsed(3 * 3_600_000)).toBe('3h');
    expect(formatElapsed(3 * 3_600_000 + 5 * 60_000)).toBe('3h 5m');
    expect(formatElapsed(52 * 3_600_000)).toBe('2d 4h');
  });

  it('measures times on the desktop clock plus the time the client held the reading', () => {
    expect(timingLine('Running', glanceTimings(running, 30_000)))
      .toBe('Running for 12m · session 2h · last activity 38s ago');
    // No session start yet: the session age falls back to the node's age.
    expect(timingLine('Ready', glanceTimings(unsupported))).toBe('Ready for 5m · session 2h 6m');
  });

  it("reads a running node's latest tool call as current work and otherwise as history", () => {
    expect(activityHeadline(running)).toBe('Now: Bash · npm test');
    expect(activityHeadline({ ...running, status: 'ready' })).toBe('Last: Bash · npm test');
    expect(activityHeadline(unsupported)).toBeNull();
    expect(toolCountLine(running)).toBe('14 tool calls since the prompt');
    expect(toolCountLine(unsupported)).toBeNull();
    expect(unavailableCopy('unsupported')).toBe("This harness doesn't keep a transcript Buildmesh can read.");
  });
});

describe('desktop sidebar hover card', () => {
  const node: AgentNode = {
    id: 7, mesh_id: 3, name: 'fix-login', path: '/tmp/m', branch: 'main', env: 'wsl',
    provider: 'anthropic', status: 'running', use_worktree: false, created_at: '2026-01-01',
  };
  beforeEach(() => {
    vi.useFakeTimers();
    getNodeGlance.mockReset();
    useAgentNodeStore.setState({ activeNodeId: null });
  });
  afterEach(() => vi.useRealTimers());

  const row = () => document.querySelector('[data-session-id="7"]') as HTMLElement;

  it('reads nothing until a mouse rests on the row, then shows the glance', async () => {
    getNodeGlance.mockResolvedValue(running);
    render(<NodeItem node={node} meshColor={getMeshColor(3)} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    expect(getNodeGlance).not.toHaveBeenCalled();
    fireEvent.pointerEnter(row(), { pointerType: 'mouse' });
    await act(async () => { await vi.advanceTimersByTimeAsync(499); });
    expect(getNodeGlance).not.toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(1); });
    expect(getNodeGlance).toHaveBeenCalledWith(7);
    const card = screen.getByRole('tooltip');
    expect(row().getAttribute('aria-describedby')).toBe(card.id);
    expect(card.textContent).toContain('Now: Bash · npm test');
    expect(card.textContent).toContain('The redirect drops the query string');
    expect(card.textContent).toContain('Fix the login redirect');
  });

  it('does not open for touch, closes when the pointer leaves, and stops reading', async () => {
    getNodeGlance.mockResolvedValue(running);
    render(<NodeItem node={node} meshColor={getMeshColor(3)} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    fireEvent.pointerEnter(row(), { pointerType: 'touch' });
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(screen.queryByRole('tooltip')).toBeNull();

    fireEvent.pointerEnter(row(), { pointerType: 'mouse' });
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    expect(screen.getByRole('tooltip')).toBeTruthy();
    fireEvent.pointerLeave(row());
    await act(async () => { await vi.advanceTimersByTimeAsync(150); });
    expect(screen.queryByRole('tooltip')).toBeNull();
    const reads = getNodeGlance.mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(20_000); });
    expect(getNodeGlance.mock.calls.length).toBe(reads);
  });

  it('names why there is no transcript layer', async () => {
    getNodeGlance.mockResolvedValue(unsupported);
    render(<NodeItem node={{ ...node, status: 'ready' }} meshColor={getMeshColor(3)} onSelectNode={() => {}} onDeleteNode={() => {}} />);
    fireEvent.pointerEnter(row(), { pointerType: 'mouse' });
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    expect(screen.getByRole('tooltip').textContent).toContain("doesn't keep a transcript");
  });
});

describe('phone node screen', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('reads the glance route for the node on screen and shows what the agent is doing', async () => {
    const fetcher = vi.fn(() => Promise.resolve(new Response(JSON.stringify(running), { status: 200 })));
    vi.stubGlobal('fetch', fetcher);
    render(<NodeGlancePanel nodeId={7} onAuthFailed={vi.fn()} />);
    expect(await screen.findByText('Now: Bash · npm test')).toBeTruthy();
    expect(fetcher.mock.calls[0][0]).toBe('/api/nodes/7/glance');
    expect(screen.getByText('Edit · src/auth/redirect.ts')).toBeTruthy();
    expect(screen.getByText('The redirect drops the query string; fixing it now.')).toBeTruthy();
    expect(screen.getByText('Fix the login redirect')).toBeTruthy();
  });

  it('says so when the desktop cannot answer', async () => {
    vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(new Response('{"error":"not found"}', { status: 404 }))));
    render(<NodeGlancePanel nodeId={7} onAuthFailed={vi.fn()} />);
    expect(await screen.findByText(/Could not read what this agent is doing/)).toBeTruthy();
  });
});
