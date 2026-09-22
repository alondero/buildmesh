import { defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

export default {
  // Layout fixtures are already-started sessions; no mock agent spawn races.
  list_agent_nodes: defaultFixtures.list_agent_nodes.map(node => ({ ...node, mesh_id: 1, position: node.id - 1, status: 'ready' })),
};
