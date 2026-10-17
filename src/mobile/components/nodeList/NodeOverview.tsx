
import type { AgentNode, Mesh, NodeCircuitRun, Provider } from '../../api';

import { nodeInputContext } from '../../../lib/status';
import { SectionHeading } from './SectionHeading';
import { NodeRow } from './NodeRow';

export function NodeOverview({ attentionCount, runningCount, visibleNodes, problemNodes, meshes, providers, circuitRuns, setFilter, changeView, onOpenNode }: {
  attentionCount: number; runningCount: number; visibleNodes: AgentNode[]; problemNodes: AgentNode[]; meshes: Mesh[]; providers: Provider[]; circuitRuns: Map<number, NodeCircuitRun>;
  setFilter: (filter: string) => void; changeView: (view: 'overview' | 'work' | 'capture') => void; onOpenNode: (node: AgentNode, prompt?: string) => void;
}) {
  return <>
                  <div className="overview-stats">
                    <button
                      onClick={() => {
                        setFilter("attention");
                        changeView("work");
                      }}
                    >
                      <strong
                        className={
                          attentionCount
                            ? "needs-you"
                            : ""
                        }
                      >
                        {attentionCount}
                      </strong>
                      <span>Attention</span>
                    </button>
                    <button
                      onClick={() => {
                        setFilter("running");
                        changeView("work");
                      }}
                    >
                      <strong>{runningCount}</strong>
                      <span>Running</span>
                    </button>
                    <button
                      onClick={() => {
                        setFilter("all");
                        changeView("work");
                      }}
                    >
                      <strong>{visibleNodes.length}</strong>
                      <span>Total agents</span>
                    </button>
                  </div>
                  <button
                    className="capture-shortcut"
                    onClick={() => changeView("capture")}
                  >
                    <span className="capture-plus" aria-hidden="true">
                      +
                    </span>
                    <span>
                      <strong>Capture an idea</strong>
                    </span>
                    <span aria-hidden="true">→</span>
                  </button>
                  {attentionCount === 0 && (
                    <div className="all-clear">
                      <strong>No errors, warnings, or pending replies.</strong>
                    </div>
                  )}
                  {problemNodes.length > 0 && (
                    <section aria-label="Problems" className="problem-section">
                      <SectionHeading color="var(--red)">
                        Check these first
                      </SectionHeading>
                      {problemNodes.map((node) => (
                        <div key={node.id} className="problem-item">
                          <NodeRow
                            node={node}
                            meshName={
                              meshes.find((mesh) => mesh.id === node.mesh_id)
                                ?.name
                            }
                            providers={providers}
                            circuitRun={circuitRuns.get(node.id)}
                            onClick={() =>
                              onOpenNode(node, nodeInputContext(node))
                            }
                          />
                          <p>
                            {node.status === "error"
                              ? "Agent reported an error. Open details to investigate."
                              : "Status reporting needs checking. Activity may be out of date."}
                          </p>
                        </div>
                      ))}
                    </section>
                  )}

  </>;
}
