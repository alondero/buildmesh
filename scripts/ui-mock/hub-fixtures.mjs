/**
 * A hub-shaped fixture set for the idle animation probe (issue #2599).
 *
 * The issue's reproduction scenario is a long-running hub: sixteen agent nodes
 * in mixed states, each one carrying an infinite animation — the status pulse
 * of a node that needs attention, the attention ring around its card, the
 * Circuit comet of a node that is piloting. Those animations are what make
 * the renderer Compositor produce a frame every vsync, so the count of
 * animated elements is what the measurement turns on.
 *
 * The default fixtures carry three nodes, which is enough to show the gate
 * working but not enough to show the shape of the cost this issue is about.
 * This file overrides the node list and leaves the rest of the wire responses
 * (`scripts/ui-mock/tauri-mock.mjs` merges it over the defaults).
 */

/** The statuses the scenario mixes, and what each one animates. */
const STATUSES = [
  'running', 'awaiting_input', 'pending', 'idle', 'running', 'awaiting_input',
  'running', 'spawning', 'pending', 'running', 'awaiting_input', 'idle',
  'running', 'running', 'awaiting_input', 'pending',
];

const nodeDefaults = {
  env: 'windows',
  provider: 'anthropic',
  cli_session_id: null,
  worktree_name: null,
  use_worktree: true,
  source_issue: null,
  source_pr: null,
  head_repo_owner: null,
  head_repo_clone_url: null,
  source_pr_pinned_sha: null,
  is_pinned: false,
  signal_health: null,
  worktree_path: null,
  created_at: '1970-01-01T00:00:00Z',
};

export default function hubFixtures() {
  return {
    list_agent_nodes: STATUSES.map((status, index) => ({
      ...nodeDefaults,
      id: index + 1,
      mesh_id: index % 2 === 0 ? 1 : 2,
      name: `hub-node-${String(index + 1).padStart(2, '0')}`,
      path: index % 2 === 0 ? '/home/user/buildmesh' : '/home/user/playground',
      branch: 'origin/main',
      status,
      position: index,
    })),
  };
}
