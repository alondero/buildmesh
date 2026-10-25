import { defaultFixtures } from '../../scripts/ui-mock/tauri-mock.mjs';

const configuration = { id: 'launch/parser-review', name: 'Parser review configuration', spawn_option_id: 'anthropic', model: null, effort: null, extra_args: null };
export default {
  list_agent_nodes: [{ ...defaultFixtures.list_agent_nodes[0], name: 'Fix parser', status: 'ready' }],
  list_spawn_configurations: [configuration],
  list_providers: [
    ...defaultFixtures.list_providers,
    { ...defaultFixtures.list_providers[0], id: configuration.id, label: configuration.name, configuration },
  ],
  list_circuits: [],
  trigger_circuit_from_node: 91,
};
