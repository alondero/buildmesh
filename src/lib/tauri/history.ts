import { _invoke } from './_invoke';
import type { AgentNode } from '../../types/generated/AgentNode';

export const listAgentHistory = (): Promise<AgentNode[]> => _invoke('list_agent_history');
export const reopenAgentNode = (nodeId: number): Promise<AgentNode> => _invoke('reopen_agent_node', { nodeId });
