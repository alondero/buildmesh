
import type { AgentNode, Mesh, NodeCircuitRun, Provider } from '../../api';

import { PulseDots } from '../../ui';

import { nodeInputContext } from '../../../lib/status';
import { NodeRow } from './NodeRow';

export function MeshNodeSection({ mesh, meshNodes, creating, providers, circuitRuns, setPickerMeshId, setMeshActions, onOpenNode, onOpenIssues, onOpenAgentNodes, onOpenPulls }: {
  mesh: Mesh; meshNodes: AgentNode[]; creating: number | null; providers: Provider[]; circuitRuns: Map<number, NodeCircuitRun>;
  setPickerMeshId: (id: number) => void; setMeshActions: (mesh: Mesh) => void; onOpenNode: (node: AgentNode, prompt?: string) => void;
  onOpenIssues: (mesh: Mesh) => void; onOpenAgentNodes: (mesh: Mesh) => void; onOpenPulls?: (mesh: Mesh) => void;
}) {
  return (
                  <section key={mesh.id} style={{ marginBottom: 4 }}>
                    <div
                      style={{
                        display: "flex",
                        alignItems: "center",
                        padding: "10px 12px 6px",
                        gap: 8,
                      }}
                    >
                      <span
                        style={{
                          fontSize: 10,
                          fontWeight: 600,
                          color: "var(--text-faint)",
                          textTransform: "uppercase",
                          letterSpacing: "0.05em",
                          flex: 1,
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                        }}
                      >
                        {mesh.name}
                      </span>
                      <button
                        onClick={() => setPickerMeshId(mesh.id)}
                        aria-label={`New node in ${mesh.name}`}
                        data-testid={`new-node-${mesh.id}`}
                        className="chip-btn"
                        style={{
                          width: 38,
                          padding: "8px 0",
                          textAlign: "center",
                          fontSize: 16,
                          lineHeight: 1,
                        }}
                      >
                        +
                      </button>
                      <button
                        onClick={() => setMeshActions(mesh)}
                        aria-label={`More actions for ${mesh.name}`}
                        data-testid={`mesh-actions-${mesh.id}`}
                        className="chip-btn"
                        style={{
                          width: 38,
                          padding: "8px 0",
                          textAlign: "center",
                          fontSize: 14,
                          lineHeight: 1,
                        }}
                      >
                        ⋯
                      </button>
                    </div>

                    <div className="mesh-shortcuts">
                      {onOpenPulls && <button className="chip-btn" onClick={() => onOpenPulls(mesh)}>Pull requests</button>}
                      <button
                        className="chip-btn"
                        onClick={() => onOpenIssues(mesh)}
                      >
                        Issues
                      </button>
                      <button
                        className="chip-btn"
                        onClick={() => onOpenAgentNodes(mesh)}
                      >
                        Archive
                      </button>
                    </div>
                    {creating === mesh.id ? (
                      <div
                        style={{
                          display: "flex",
                          alignItems: "center",
                          gap: 8,
                          fontSize: 12,
                          color: "var(--accent)",
                          padding: "8px 12px 12px",
                        }}
                      >
                        <PulseDots /> Creating node…
                      </div>
                    ) : meshNodes.length === 0 ? (
                      <div
                        style={{
                          fontSize: 12,
                          color: "var(--text-faint)",
                          padding: "8px 12px 12px",
                          fontStyle: "italic",
                        }}
                      >
                        No other agents in this mesh — tap + to start one
                      </div>
                    ) : (
                      meshNodes.map((node) => (
                        <NodeRow
                          key={node.id}
                          node={node}
                          meshName={mesh.name}
                          onClick={() =>
                            onOpenNode(node, nodeInputContext(node))
                          }
                          providers={providers}
                          circuitRun={circuitRuns.get(node.id)}
                        />
                      ))
                    )}
                  </section>

  );
}
