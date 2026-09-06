/**
 * Exit guard pure helpers (issue #1501) — TDD seam for the window-close
 * confirmation flow.
 *
 * Decides which agent nodes count as "active" for the exit prompt and which
 * of those are non-resumable (will lose progress on quit).
 */
import { describe, it, expect } from 'vitest';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';
import {
  ACTIVE_EXIT_STATUSES,
  isActiveForExit,
  getActiveExitNodes,
  parseExitHarnessId,
  buildSupportsResumeMap,
  isExitNodeResumable,
  partitionExitNodes,
  shouldConfirmExit,
  formatExitBody,
} from '../../src/lib/exitGuard';

function node(overrides: Partial<AgentNode>): AgentNode {
  return {
    id: 1,
    mesh_id: 1,
    name: 'node',
    path: '/repo',
    branch: 'main',
    env: 'Windows',
    provider: 'claude',
    status: 'running',
    cli_session_id: 'sess-1',
    worktree_name: null,
    use_worktree: false,
    is_pinned: false,
    source_issue: null,
    source_pr: null,
    head_repo_owner: null,
    head_repo_clone_url: null,
    source_pr_pinned_sha: null,
    signal_health: null,
    position: 0,
    created_at: '2026-01-01T00:00:00Z',
    worktree_path: null,
    ...overrides,
  } as AgentNode;
}

function provider(overrides: Partial<ProviderInfo>): ProviderInfo {
  return {
    id: 'claude',
    label: 'Claude Code',
    color: '#000',
    icon: 'C',
    resumable: true,
    harness_id: 'claude',
    provider_id: null,
    is_proxied: false,
    group_key: 'claude',
    capabilities: {
      harness_id: 'claude',
      supports_resume: true,
      auto_resume_on_startup: true,
      requires_attention_hook: false,
      attention_capability: null,
      supports_passive_turn_watcher: false,
      produces_readable_transcript: true,
      supports_model_override: true,
      supports_effort_override: true,
      supports_extra_args: true,
      supports_prefill: true,
      is_plain_terminal: false,
      effort_control: { kind: 'closed', allowed: ['low'] },
    },
    ...overrides,
  } as unknown as ProviderInfo;
}

describe('exitGuard (issue #1501)', () => {
  it('defines the active statuses as running/awaiting_input/spawning/ready', () => {
    expect([...ACTIVE_EXIT_STATUSES].sort()).toEqual(
      ['awaiting_input', 'ready', 'running', 'spawning'].sort(),
    );
  });

  it('isActiveForExit matches only the four active statuses', () => {
    for (const s of ['running', 'awaiting_input', 'spawning', 'ready'] as const) {
      expect(isActiveForExit(s)).toBe(true);
    }
    for (const s of ['idle', 'suspended', 'error', 'archived', 'completed', 'pending'] as const) {
      expect(isActiveForExit(s)).toBe(false);
    }
  });

  it('getActiveExitNodes filters to active nodes only', () => {
    const nodes = [
      node({ id: 1, status: 'running' }),
      node({ id: 2, status: 'idle' }),
      node({ id: 3, status: 'awaiting_input' }),
      node({ id: 4, status: 'suspended' }),
      node({ id: 5, status: 'ready' }),
    ];
    expect(getActiveExitNodes(nodes).map((n) => n.id)).toEqual([1, 3, 5]);
  });

  it('parseExitHarnessId splits composite ids and maps empty to anthropic', () => {
    expect(parseExitHarnessId('claude')).toBe('claude');
    expect(parseExitHarnessId('claude:minimax')).toBe('claude');
    expect(parseExitHarnessId('terminal')).toBe('terminal');
    expect(parseExitHarnessId('')).toBe('anthropic');
  });

  it('buildSupportsResumeMap prefers the native row and falls back to first', () => {
    const terminalCaps = { ...provider().capabilities, harness_id: 'terminal', supports_resume: false, is_plain_terminal: true };
    const map = buildSupportsResumeMap([
      provider({ id: 'terminal', harness_id: 'terminal', capabilities: terminalCaps as never }),
      provider({ id: 'claude', harness_id: 'claude' }),
    ]);
    expect(map.get('terminal')).toBe(false);
    expect(map.get('claude')).toBe(true);
    expect(map.get('unknown')).toBeUndefined();
  });

  it('isExitNodeResumable requires a session id AND supports_resume', () => {
    const map = new Map([['claude', true], ['terminal', false]]);
    expect(isExitNodeResumable(node({ provider: 'claude', cli_session_id: 'abc' }), map)).toBe(true);
    // Fresh agent without a session id is non-resumable even on a resumable harness.
    expect(isExitNodeResumable(node({ provider: 'claude', cli_session_id: null }), map)).toBe(false);
    expect(isExitNodeResumable(node({ provider: 'claude', cli_session_id: '' }), map)).toBe(false);
    // Terminal harness never resumes.
    expect(isExitNodeResumable(node({ provider: 'terminal', cli_session_id: 'abc' }), map)).toBe(false);
    // Unknown harness is fail-closed (warn).
    expect(isExitNodeResumable(node({ provider: 'nope', cli_session_id: 'abc' }), map)).toBe(false);
  });

  it('treats the anthropic/claude legacy alias twins as the same executor', () => {
    // Empty provider normalises to anthropic; the live menu may key the
    // same executor as claude (post-#538). Either twin resuming is enough.
    expect(
      isExitNodeResumable(node({ provider: '', cli_session_id: 'abc' }), new Map([['claude', true]])),
    ).toBe(true);
    expect(
      isExitNodeResumable(node({ provider: 'claude', cli_session_id: 'abc' }), new Map([['anthropic', true]])),
    ).toBe(true);
    expect(
      isExitNodeResumable(node({ provider: '', cli_session_id: 'abc' }), new Map([['terminal', false]])),
    ).toBe(false);
  });

  it('partitionExitNodes splits resumable from non-resumable', () => {
    const map = new Map([['claude', true], ['terminal', false]]);
    const active = [
      node({ id: 1, name: 'a', provider: 'claude', cli_session_id: 's1' }),
      node({ id: 2, name: 'b', provider: 'claude', cli_session_id: null }),
      node({ id: 3, name: 'c', provider: 'terminal', cli_session_id: 's3' }),
    ];
    const { resumable, nonResumable } = partitionExitNodes(active, map);
    expect(resumable.map((n) => n.id)).toEqual([1]);
    expect(nonResumable.map((n) => n.id)).toEqual([2, 3]);
  });

  it('shouldConfirmExit requires active nodes AND the preference', () => {
    expect(shouldConfirmExit([], true)).toBe(false);
    expect(shouldConfirmExit([node({})], false)).toBe(false);
    expect(shouldConfirmExit([node({})], true)).toBe(true);
  });

  it('formatExitBody uses the spec copy with singular/plural session(s)', () => {
    expect(formatExitBody(1)).toBe('You have 1 active agent session(s) running.');
    expect(formatExitBody(2)).toBe('You have 2 active agent session(s) running.');
  });
});
