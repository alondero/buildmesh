import type { AgentNode } from '../../../src/types/generated/AgentNode';
import type { SessionStatus } from '../../../src/types/generated/SessionStatus';

/**
 * Test helper — a minimal-but-valid `AgentNode` factory for shortcut tests
 * that seed the store and then fire an action.
 *
 * Spread defaults keep new generated fields from breaking these tests: only
 * the fields a test actually cares about need overriding. Same precedent as
 * the local `makeNode` factories in `tests/unit/grid-shortcuts.test.ts` and
 * `tests/unit/awaiting-input-shortcuts.test.ts`, factored out for the
 * table-driven dispatch tests (issue #2240) so four files don't each carry
 * their own copy of the shape.
 */
export function makeAgentNode(
  overrides: Partial<AgentNode> & Pick<AgentNode, 'id' | 'mesh_id' | 'position'>,
): AgentNode {
  return {
    name: `agent-${overrides.id}`,
    path: '/repo',
    branch: 'main',
    env: 'wsl',
    provider: 'claude',
    status: 'running' as SessionStatus,
    use_worktree: false,
    created_at: '2026-07-19T00:00:00Z',
    scratchpad: '',
    sandbox: false,
    cli_session_id: null,
    worktree_name: null,
    source_issue: null,
    archived: false,
    head_repo_owner: null,
    head_repo_clone_url: null,
    source_pr_pinned_sha: null,
    ...overrides,
  };
}