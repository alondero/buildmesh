import { defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

export default {
  list_agent_nodes: defaultFixtures.list_agent_nodes.map(node => ({ ...node, mesh_id: 1, position: node.id - 1 })),
};
