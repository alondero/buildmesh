/**
 * CircuitFlowEditor — the Autopilot Circuits canvas (issue #1209).
 *
 * Full-screen React Flow surface replacing the Probe tab's throwaway
 * authoring form. The working copy lives as React Flow nodes/edges;
 * the blueprint AST is *derived* at save time (`toGraph`) so the
 * canonical shape stays the Rust one and persists whole via
 * `update_circuit_graph`. Positions are session-only (Dagre-derived);
 * `graph_json` never stores layout.
 *
 * Run observation rides the same canvas: the selected run's steps
 * pulse/check/alert node cards in place, blocked gates approve
 * without leaving, and traversed edges glow cyan.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Background,
  BackgroundVariant,
  Controls,
  ReactFlow,
  ReactFlowProvider,
  useEdgesState,
  useNodesState,
  useReactFlow,
  type Connection,
  type Edge,
  type OnConnectEnd,
} from '@xyflow/react';
import '@xyflow/react/dist/style.css';

import type { AutopilotCircuit } from '../../lib/tauri';
import type { CircuitRunDetail } from '../../lib/tauri';
import { approveCircuitStep, updateCircuitGraph } from '../../lib/tauri';
import { formatError } from '../../lib/errorUtils';
import type { CircuitGraph } from '../../types/generated/CircuitGraph';
import type { CircuitNode } from '../../types/generated/CircuitNode';
import type { CircuitNodeKind } from '../../types/generated/CircuitNodeKind';
import type { EdgeCondition } from '../../types/generated/EdgeCondition';
import { CircuitNodeCard, type CircuitFlowNode } from './CircuitNodeCard';
import { OutcomeEdge, nextCondition } from './OutcomeEdge';
import { NodePalette, PALETTE_MIME } from './NodePalette';
import { QuickConnectMenu } from './QuickConnectMenu';
import { InspectorPanel } from './InspectorPanel';
import { RunHistoryDrawer } from './RunHistoryDrawer';
import {
  NODE_SPECS,
  conditionFromHandle,
  defaultKind,
  edgeKey,
  layoutPositions,
  makeNodeId,
  parseGraph,
  sourceOutcomes,
  specFor,
  toGraph,
  traversedEdgeKeys,
  type StepLike,
} from './circuitGraphModel';

// Stable component identities — redefining these per render would make
// React Flow remount every card on any state change.
const nodeTypes = { circuit: CircuitNodeCard };
const edgeTypes = { outcome: OutcomeEdge };

interface EditorNodeDataSeed {
  graph: CircuitGraph;
  positions: Map<string, { x: number; y: number }>;
}

function seedNodes({ graph, positions }: EditorNodeDataSeed): CircuitFlowNode[] {
  return graph.nodes.map((n) => ({
    id: n.id,
    type: 'circuit' as const,
    position: positions.get(n.id) ?? { x: 0, y: 0 },
    data: { circuitNode: n },
  }));
}

function seedEdges(graph: CircuitGraph, highlighted: Set<string>): Edge[] {
  return graph.edges.map((e) => ({
    id: edgeKey(e),
    source: e.from,
    target: e.to,
    type: 'outcome' as const,
    sourceHandle: e.condition === 'always' ? null : e.condition.on_outcome,
    data: { condition: e.condition, highlight: highlighted.has(edgeKey(e)) },
  }));
}

interface CircuitFlowEditorProps {
  circuit: AutopilotCircuit;
  runs: CircuitRunDetail[];
  onClose: () => void;
  /** Fired after a successful save so parents can refetch. */
  onSaved?: () => void;
}

export function CircuitFlowEditor(props: CircuitFlowEditorProps) {
  return (
    <ReactFlowProvider>
      <CircuitFlowEditorInner {...props} />
    </ReactFlowProvider>
  );
}

function CircuitFlowEditorInner({ circuit, runs, onClose, onSaved }: CircuitFlowEditorProps) {
  const { screenToFlowPosition } = useReactFlow();
  const wrapperRef = useRef<HTMLDivElement>(null);

  // Working copy: React Flow state IS the editor's source of truth;
  // the AST is derived on save (`toGraph`).
  const initial = useMemo(() => parseGraph(circuit.graph_json), [circuit.graph_json]);
  const [nodes, setNodes, onNodesChange] = useNodesState<CircuitFlowNode>(
    seedNodes({ graph: initial, positions: layoutPositions(initial, 'LR') })
  );
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>(seedEdges(initial, new Set()));

  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const [showRunDrawer, setShowRunDrawer] = useState(false);
  const [selectedRunId, setSelectedRunId] = useState<number | null>(null);
  const [quickConnect, setQuickConnect] = useState<{
    fromNodeId: string;
    fromHandleId: string | null;
    position: { x: number; y: number };
  } | null>(null);
  const [editorError, setEditorError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  // Default observed run: newest active one, else newest overall.
  useEffect(() => {
    if (runs.length === 0) {
      setSelectedRunId(null);
      return;
    }
    setSelectedRunId((prev) => {
      if (prev !== null && runs.some((r) => r.run.id === prev)) return prev;
      const active = runs.find((r) => r.run.state === 'running' || r.run.state === 'paused');
      return (active ?? runs[0]).run.id;
    });
  }, [runs]);

  const selectedRun = runs.find((r) => r.run.id === selectedRunId) ?? null;
  const stepByNodeId = useMemo(() => {
    const m = new Map<string, StepLike>();
    if (selectedRun) {
      for (const s of selectedRun.steps) m.set(s.node_id, s);
    }
    return m;
  }, [selectedRun]);
  // Content-addressed topology so highlight recomputes on real edits
  // only — keying this memo on the raw arrays would loop with the
  // highlight-sync effect below.
  const topologyJson = useMemo(() => JSON.stringify(toGraph(nodes, edges)), [nodes, edges]);
  const highlightedKeys = useMemo(
    () =>
      traversedEdgeKeys(
        selectedRun?.steps ?? [],
        (JSON.parse(topologyJson) as CircuitGraph).edges
      ),
    [selectedRun, topologyJson]
  );

  const currentNode = useMemo(
    () => nodes.find((n) => n.id === selectedNodeId)?.data.circuitNode ?? null,
    [nodes, selectedNodeId]
  );

  const handleApprove = useCallback(async (runId: number, nodeId: string) => {
    try {
      await approveCircuitStep(runId, nodeId);
    } catch (err) {
      console.error('Failed to approve circuit step:', err);
      setEditorError(formatError(err));
    }
  }, []);

  // Latest nodes for stable callbacks (edge badges cycle conditions
  // based on the SOURCE node's outcome vocabulary).
  const nodesRef = useRef(nodes);
  useEffect(() => {
    nodesRef.current = nodes;
  }, [nodes]);

  /** Cycle one edge's condition and re-key it to match. */
  const cycleEdge = useCallback(
    (edgeId: string) => {
      setEdges((es) =>
        es.map((e) => {
          if (e.id !== edgeId) return e;
          const current: EdgeCondition =
            (e.data?.condition as EdgeCondition | undefined) ?? 'always';
          const source = nodesRef.current.find((n) => n.id === e.source);
          const outcomes =
            source != null ? sourceOutcomes(source.data.circuitNode.type) : null;
          const outcomeCycle: EdgeCondition[] = (outcomes ?? ['completed']).map(
            (o) => ({ on_outcome: o })
          );
          const next = nextCondition(current, outcomeCycle);
          const nextKey = edgeKey({
            from: e.source,
            to: e.target,
            condition: next,
          });
          return {
            ...e,
            id: nextKey,
            sourceHandle: next === 'always' ? null : next.on_outcome,
            data: { ...e.data, condition: next },
          };
        })
      );
    },
    [setEdges]
  );

  // Keep edge highlight/badge wiring in sync with the selected run.
  useEffect(() => {
    setEdges((eds) => {
      let changed = false;
      const next = eds.map((e) => {
        const highlight = highlightedKeys.has(String(e.id));
        if ((e.data?.highlight ?? false) === highlight && e.data?.onCycle === cycleEdge) return e;
        changed = true;
        return { ...e, data: { ...e.data, highlight, onCycle: cycleEdge } };
      });
      return changed ? next : eds;
    });
  }, [highlightedKeys, cycleEdge, setEdges]);

  // Push the selected run's per-node status into the card overlays
  // (pulse / check / blocked-approve / error tooltip).
  useEffect(() => {
    setNodes((ns) =>
      ns.map((n) => {
        const step = stepByNodeId.get(n.id);
        const blockedRunId =
          step?.status === 'blocked' && selectedRunId !== null ? selectedRunId : undefined;
        return { ...n, data: { ...n.data, step, blockedRunId, onApprove: handleApprove } };
      })
    );
  }, [stepByNodeId, selectedRunId, handleApprove, setNodes]);

  const addNodeAt = useCallback(
    (discriminator: string, position: { x: number; y: number }) => {
      const id = makeNodeId(discriminator, nodes.map((n) => n.id));
      const circuitNode: CircuitNode = { id, type: defaultKind(discriminator) };
      setNodes((ns) => [
        ...ns,
        { id, type: 'circuit', position, data: { circuitNode } },
      ]);
      setSelectedNodeId(id);
    },
    [nodes, setNodes]
  );

  const addWiredNode = useCallback(
    (discriminator: string, fromNodeId: string, fromHandleId: string | null, position: { x: number; y: number }) => {
      const source = nodes.find((n) => n.id === fromNodeId);
      const condition = conditionFromHandle(source?.data.circuitNode.type, fromHandleId);
      const id = makeNodeId(discriminator, nodes.map((n) => n.id));
      const circuitNode: CircuitNode = { id, type: defaultKind(discriminator) };
      const key = edgeKey({ from: fromNodeId, to: id, condition });
      setNodes((ns) => [...ns, { id, type: 'circuit', position, data: { circuitNode } }]);
      setEdges((es) => [
        ...es,
        {
          id: key,
          source: fromNodeId,
          target: id,
          sourceHandle: fromHandleId,
          type: 'outcome',
          data: { condition, highlight: false },
        },
      ]);
      setSelectedNodeId(id);
    },
    [nodes, setEdges, setNodes]
  );

  // Drag-to-search: a connection released over empty canvas opens the
  // fuzzy-search menu instead of wiring nothing.
  const onConnectEnd: OnConnectEnd = useCallback(
    (event, connectionState) => {
      if (connectionState.isValid) return;
      const fromNodeId = connectionState.fromNode?.id;
      if (!fromNodeId) return;
      const { clientX, clientY } = event as MouseEvent;
      const position = screenToFlowPosition({ x: clientX, y: clientY });
      setQuickConnect({
        fromNodeId,
        fromHandleId: connectionState.fromHandle?.id ?? null,
        position,
      });
    },
    [screenToFlowPosition]
  );

  const onConnect = useCallback(
    (connection: Connection) => {
      const source = nodes.find((n) => n.id === connection.source);
      const condition = conditionFromHandle(
        source?.data.circuitNode.type,
        connection.sourceHandle ?? null
      );
      const key = edgeKey({ from: connection.source, to: connection.target, condition });
      setEdges((es) => [
        ...es.filter((e) => e.id !== key),
        {
          id: key,
          source: connection.source,
          target: connection.target,
          sourceHandle: connection.sourceHandle,
          type: 'outcome',
          data: { condition, highlight: false },
        },
      ]);
    },
    [nodes, setEdges]
  );

  const onDrop = useCallback(
    (event: React.DragEvent) => {
      event.preventDefault();
      const discriminator = event.dataTransfer.getData(PALETTE_MIME);
      if (!NODE_SPECS.some((s) => s.discriminator === discriminator)) return;
      const position = screenToFlowPosition({ x: event.clientX, y: event.clientY });
      addNodeAt(discriminator, { x: position.x - 110, y: position.y - 32 });
    },
    [addNodeAt, screenToFlowPosition]
  );

  const autoLayout = (direction: 'LR' | 'TB') => {
    const positions = layoutPositions(toGraph(nodes, edges), direction);
    setNodes((ns) => ns.map((n) => ({ ...n, position: positions.get(n.id) ?? n.position })));
  };

  const updateSelectedKind = (kind: CircuitNodeKind) => {
    if (selectedNodeId === null) return;
    setNodes((ns) =>
      ns.map((n): CircuitFlowNode => {
        if (n.id !== selectedNodeId) return n;
        const circuitNode: CircuitNode = { id: n.id, type: kind };
        return { ...n, data: { ...n.data, circuitNode } };
      })
    );
  };

  const deleteSelectedNode = () => {
    if (selectedNodeId === null) return;
    setNodes((ns) => ns.filter((n) => n.id !== selectedNodeId));
    setEdges((es) => es.filter((e) => e.source !== selectedNodeId && e.target !== selectedNodeId));
    setSelectedNodeId(null);
  };

  const savedJson = useMemo(() => JSON.stringify(parseGraph(circuit.graph_json)), [circuit.graph_json]);
  const dirty = useMemo(() => JSON.stringify(toGraph(nodes, edges)) !== savedJson, [nodes, edges, savedJson]);

  const handleSave = async () => {
    setSaving(true);
    setEditorError(null);
    try {
      await updateCircuitGraph(circuit.id, JSON.stringify(toGraph(nodes, edges)));
      onSaved?.();
    } catch (err) {
      console.error('Failed to save circuit graph:', err);
      setEditorError(formatError(err));
    } finally {
      setSaving(false);
    }
  };

  // Esc closes (menus clear first via their own Escape handlers).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div
      className="absolute inset-0 z-40 flex flex-col bg-bg-base"
      data-testid="circuit-flow-editor"
    >
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-1.5 border-b border-border-subtle bg-bg-surface shrink-0">
        <span className="text-sm font-semibold text-text-primary">{circuit.name}</span>
        {dirty && (
          <span className="text-2xs text-status-warning" data-testid="editor-dirty">
            unsaved
          </span>
        )}
        <div className="flex-1" />
        <button
          type="button"
          onClick={() => autoLayout('LR')}
          data-testid="autolayout-lr"
          className="px-2 py-0.5 rounded-md bg-text-muted/10 text-text-muted hover:text-text-primary text-xs"
        >
          Layout →
        </button>
        <button
          type="button"
          onClick={() => autoLayout('TB')}
          data-testid="autolayout-tb"
          className="px-2 py-0.5 rounded-md bg-text-muted/10 text-text-muted hover:text-text-primary text-xs"
        >
          Layout ↓
        </button>
        <button
          type="button"
          onClick={() => setShowRunDrawer((v) => !v)}
          data-testid="toggle-run-history"
          className={`px-2 py-0.5 rounded-md text-xs ${
            showRunDrawer
              ? 'bg-accent-cyan/25 text-accent-cyan'
              : 'bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25'
          }`}
        >
          Run History
        </button>
        {(editorError !== null) && (
          <span className="text-xs text-status-error" role="alert" data-testid="editor-error">
            {editorError}
          </span>
        )}
        <button
          type="button"
          onClick={() => void handleSave()}
          disabled={saving}
          data-testid="editor-save"
          className="px-2 py-0.5 rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40 text-xs"
        >
          {saving ? 'Saving…' : 'Save'}
        </button>
        <button
          type="button"
          onClick={onClose}
          data-testid="editor-close"
          aria-label="Close editor"
          className="px-2 py-0.5 rounded-md text-text-muted hover:text-status-error text-xs"
        >
          ✕
        </button>
      </div>

      {/* Canvas + side panels */}
      <div className="relative flex flex-1 overflow-hidden">
        <div className="relative flex-1" ref={wrapperRef} data-testid="canvas-wrapper">
          <ReactFlow
            nodes={nodes}
            edges={edges}
            onNodesChange={onNodesChange}
            onEdgesChange={onEdgesChange}
            onConnect={onConnect}
            onConnectEnd={onConnectEnd}
            onDrop={onDrop}
            onDragOver={(e) => {
              e.preventDefault();
              e.dataTransfer.dropEffect = 'move';
            }}
            nodeTypes={nodeTypes}
            edgeTypes={edgeTypes}
            deleteKeyCode={null}
            onNodeClick={(_, node) => setSelectedNodeId(node.id)}
            onPaneClick={() => {
              setSelectedNodeId(null);
              setQuickConnect(null);
            }}
            fitView
          >
            <Background variant={BackgroundVariant.Dots} gap={16} size={1} />
            <Controls showInteractive={false} />
          </ReactFlow>
          <NodePalette
            onAdd={(d) => {
              const rect = wrapperRef.current?.getBoundingClientRect();
              const position = screenToFlowPosition({
                x: (rect?.left ?? 0) + (rect?.width ?? 800) / 2,
                y: (rect?.top ?? 0) + (rect?.height ?? 600) / 2,
              });
              addNodeAt(d, { x: position.x - 110, y: position.y - 32 });
            }}
          />
          {quickConnect && (
            <QuickConnectMenu
              position={quickConnect.position}
              onSelect={(spec) => {
                addWiredNode(
                  spec.discriminator,
                  quickConnect.fromNodeId,
                  quickConnect.fromHandleId,
                  { x: quickConnect.position.x - 110, y: quickConnect.position.y - 32 }
                );
                setQuickConnect(null);
              }}
              onDismiss={() => setQuickConnect(null)}
            />
          )}
        </div>

        <InspectorPanel
          node={currentNode}
          onChange={updateSelectedKind}
        />
        {currentNode !== null && (
          <div className="absolute bottom-3 left-1/2 -translate-x-1/2 z-20">
            <button
              type="button"
              onClick={deleteSelectedNode}
              data-testid="inspector-delete-node"
              className="px-2 py-0.5 rounded-md bg-bg-overlay border border-border-subtle text-xs text-text-muted hover:text-status-error"
            >
              Delete “{specFor(currentNode.type.type).label}” ({currentNode.id})
            </button>
          </div>
        )}
        {showRunDrawer && (
          <RunHistoryDrawer
            runs={runs}
            selectedRunId={selectedRunId}
            onSelectRun={setSelectedRunId}
          />
        )}
      </div>
    </div>
  );
}
