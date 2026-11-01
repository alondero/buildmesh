import { defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

export const MESH_NAME = 'commerce-platform-production-release-coordination';
export const NODE_NAMES = [
  'investigate-checkout-reconciliation-primary-agent',
  'repair-checkout-reconciliation-secondary-agent',
];

export default {
  list_meshes: defaultFixtures.list_meshes.map(mesh => mesh.id === 1 ? { ...mesh, name: MESH_NAME } : mesh),
  list_agent_nodes: defaultFixtures.list_agent_nodes.map(node => ({
    ...node,
    name: NODE_NAMES[node.id - 1] ?? node.name,
    status: 'ready',
  })),
  list_pending_worktree_removals: [],
  list_directory: { name: 'buildmesh', path: '/home/user/buildmesh', is_dir: true, children: [] },
  node_changed_files: [],
};
