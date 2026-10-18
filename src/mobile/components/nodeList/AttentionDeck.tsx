import type { AgentNode, Mesh, NodeCircuitRun, Provider } from '../../api';
import { nodeInputRequest } from '../../../lib/status';
import { AttentionCard } from './AttentionCard';
import { SectionHeading } from './SectionHeading';
import type { SentAction, SentMarker } from './types';

export function AttentionDeck({ activeView, attentionNodes, meshes, circuitRuns, providers, keyBusy, keySent, sendQuickAction, onOpenNode }: {
  activeView: 'overview' | 'work' | 'capture'; attentionNodes: AgentNode[]; meshes: Mesh[]; circuitRuns: Map<number, NodeCircuitRun>; providers: Provider[];
  keyBusy: Map<number, SentAction>; keySent: Map<number, SentMarker>; sendQuickAction: (nodeId: number, requestKey: string, action: SentAction) => Promise<void>; onOpenNode: (node: AgentNode, prompt?: string) => void;
}) {
  return <>
              {activeView === "overview" && attentionNodes.length > 0 && (
                <section
                  data-testid="attention-section"
                  style={{ marginBottom: 8 }}
                >
                  <SectionHeading color="var(--amber)">
                    Waiting for input
                  </SectionHeading>
                  <div
                    className={`deck${attentionNodes.length === 1 ? " deck-single" : ""}`}
                    data-testid="attention-deck"
                  >
                    {attentionNodes.map((node) => {
                      // Every `awaiting_input` node is blocked on a request;
                      // `undefined` here would mean the filter above is lying,
                      // so skip the card rather than render reply controls the
                      // observation cannot justify (issue #1966).
                      const request = nodeInputRequest(node);
                      if (!request) return null;
                      // A marker only applies to the request it answered.
                      const marker = keySent.get(node.id);
                      return (
                        <AttentionCard
                          key={`attn-${node.id}`}
                          node={node}
                          meshName={
                            meshes.find((m) => m.id === node.mesh_id)?.name
                          }
                          request={request}
                          circuitRun={circuitRuns.get(node.id)}
                          providers={providers}
                          busy={keyBusy.get(node.id)}
                          sent={
                            marker?.requestKey === request.key
                              ? marker.action
                              : undefined
                          }
                          onApprove={() =>
                            void sendQuickAction(
                              node.id,
                              request.key,
                              "approve",
                            )
                          }
                          onReject={() =>
                            void sendQuickAction(node.id, request.key, "reject")
                          }
                          onOpen={() => onOpenNode(node, request.message)}
                        />
                      );
                    })}
                  </div>
                </section>
              )}
  </>;
}
