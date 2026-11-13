/**
 * circuitLayout — the dagre-backed auto-layout for the circuit canvas.
 *
 * Split out of `circuitGraphModel.ts` (issue #2757, bundle size). The graph
 * model is imported eagerly by the Agent Node overview tab for a handful of
 * pure helpers (`statusTextClass`, `ledgerTimestampMs`), while the layout
 * function is only used by the canvas editor. Keeping them in one module meant
 * the eager import also dragged `@dagrejs/dagre` (45.5 kB minified) into the
 * entry chunk, even though the canvas editor is a lazy surface
 * (`src/components/lazySurfaces.ts`).
 *
 * The rule this module restores: a module that is reachable from the entry
 * chunk must not import a heavy library that only a lazy surface needs.
 */
import dagre from '@dagrejs/dagre';
import type { CircuitGraph } from '../../types/generated/CircuitGraph';

/** Card size used for spacing; the real card may vary a few px. */
export const NODE_SIZE = { width: 220, height: 64 };

/**
 * Lay out the whole graph with Dagre in `direction`, returning a
 * position per node id (disconnected nodes included).
 */
export function layoutPositions(
  graph: Pick<CircuitGraph, 'nodes' | 'edges'>,
  direction: 'LR' | 'TB'
): Map<string, { x: number; y: number }> {
  const g = new dagre.graphlib.Graph();
  g.setGraph({ rankdir: direction, nodesep: 40, ranksep: 80, marginx: 20, marginy: 20 });
  g.setDefaultEdgeLabel(() => ({}));
  for (const node of graph.nodes) {
    g.setNode(node.id, { width: NODE_SIZE.width, height: NODE_SIZE.height });
  }
  for (const edge of graph.edges) {
    g.setEdge(edge.from, edge.to);
  }
  dagre.layout(g);

  const positions = new Map<string, { x: number; y: number }>();
  for (const node of graph.nodes) {
    const laid = g.node(node.id) as { x: number; y: number } | undefined;
    // Dagre centres on the node; React Flow wants the top-left corner.
    positions.set(node.id, {
      x: (laid?.x ?? 0) - NODE_SIZE.width / 2,
      y: (laid?.y ?? 0) - NODE_SIZE.height / 2,
    });
  }
  return positions;
}
