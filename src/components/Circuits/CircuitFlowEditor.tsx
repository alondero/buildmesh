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

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
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
  stableGraphJson,
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
    /** Wrapper-relative pixels — where the drag released; drives the menu's DOM placement. */
    screen: { x: number; y: number };
    /** Flow coordinates of the same point; where the new node is created. */
    flow: { x: number; y: number };
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
  const topologyJson = useMemo(() => stableGraphJson(toGraph(nodes, edges)), [nodes, edges]);
  // Stable graph object for the InspectorPanel's reachability useMemo.
  // Without this memo, `toGraph(nodes, edges)` would mint a fresh
  // object on every render, busting the panel's deps and re-walking
  // the BFS on every keystroke (issue #1359 review feedback).
  const currentGraph = useMemo(() => toGraph(nodes, edges), [nodes, edges]);
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

  /** Cycle one edge's condition and re-key it to match. If the target
   *  condition already exists on a parallel edge between the same pair,
   *  drop THIS edge instead of minting a duplicate id (React Flow
   *  requires unique edge ids). */
  const cycleEdge = useCallback(
    (edgeId: string) => {
      setEdges((es) => {
        const target = es.find((e) => e.id === edgeId);
        if (!target) return es;
        const current: EdgeCondition =
          (target.data?.condition as EdgeCondition | undefined) ?? 'always';
        const source = nodesRef.current.find((n) => n.id === target.source);
        const outcomes =
          source != null ? sourceOutcomes(source.data.circuitNode.type) : null;
        const outcomeCycle: EdgeCondition[] = (outcomes ?? ['completed']).map(
          (o) => ({ on_outcome: o })
        );
        const next = nextCondition(current, outcomeCycle);
        const nextKey = edgeKey({ from: target.source, to: target.target, condition: next });
        if (nextKey === target.id) return es;
        const cycled = {
          ...target,
          id: nextKey,
          sourceHandle: next === 'always' ? null : next.on_outcome,
          data: { ...target.data, condition: next },
        };
        // Replace this edge; swallow any parallel edge the new condition
        // would collide with.
        return es
          .filter((e) => e.id !== edgeId && e.id !== nextKey)
          .concat(cycled);
      });
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
  // (pulse / check / blocked-approve / error tooltip). Only nodes whose
  // step actually changed are re-wrapped — telemetry refetches mint new
  // step objects every tick, and re-allocating every card would make
  // React Flow re-render/re-measure the whole canvas per heartbeat.
  useEffect(() => {
    setNodes((ns) => {
      let changed = false;
      const next = ns.map((n) => {
        const step = stepByNodeId.get(n.id);
        const blockedRunId =
          step?.status === 'blocked' && selectedRunId !== null ? selectedRunId : undefined;
        const prev = n.data.step;
        const sameStep =
          prev?.node_id === step?.node_id &&
          prev?.status === step?.status &&
          prev?.outcome === step?.outcome &&
          prev?.error_message === step?.error_message &&
          prev?.started_at === step?.started_at &&
          prev?.completed_at === step?.completed_at &&
          n.data.blockedRunId === blockedRunId &&
          n.data.onApprove === handleApprove;
        if (sameStep) return n;
        changed = true;
        return { ...n, data: { ...n.data, step, blockedRunId, onApprove: handleApprove } };
      });
      return changed ? next : ns;
    });
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
  // fuzzy-search menu instead of wiring nothing. Two coordinate spaces:
  // the menu is a DOM sibling of the canvas, so it positions in
  // wrapper-relative SCREEN pixels; the created node lands at the same
  // spot expressed in FLOW coordinates (pan/zoom applied).
  const onConnectEnd: OnConnectEnd = useCallback(
    (event, connectionState) => {
      if (connectionState.isValid) return;
      const fromNodeId = connectionState.fromNode?.id;
      if (!fromNodeId) return;
      const { clientX, clientY } = event as MouseEvent;
      const rect = wrapperRef.current?.getBoundingClientRect();
      const screen = {
        x: clientX - (rect?.left ?? 0),
        y: clientY - (rect?.top ?? 0),
      };
      const flow = screenToFlowPosition({ x: clientX, y: clientY });
      setQuickConnect({
        fromNodeId,
        fromHandleId: connectionState.fromHandle?.id ?? null,
        screen,
        flow,
      });
    },
    [screenToFlowPosition]
  );

  const onConnect = useCallback(
    (connection: Connection) => {
      // Self-loops are invalid circuits (the validator rejects them too).
      if (connection.source === connection.target || !connection.target) return;
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

  const savedJson = useMemo(
    () => stableGraphJson(parseGraph(circuit.graph_json)),
    [circuit.graph_json]
  );
  const dirty = useMemo(
    () => stableGraphJson(toGraph(nodes, edges)) !== savedJson,
    [nodes, edges, savedJson]
  );

  // -- dirty guard (issue #1244) ---------------------------------------------
  // Mirror `<Modal dirty>`'s discard-banner pattern inline: a reflexive
  // Escape or ✕ click on an unsaved editor must NOT silently destroy the
  // graph (muscle memory guarantees Escape hits — the overlay sits above
  // live terminals). Only the explicit Discard button calls onClose.
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  // Mirror `dirty` into a ref so the once-armed Escape listener sees the
  // live value (without this, a dirty toggle landing after mount would be
  // invisible — captured closure still holds the mount-time `dirty = false`).
  // useLayoutEffect so the ref updates synchronously before the browser
  // commits the new render and the next Escape fires.
  const dirtyRef = useRef(dirty);
  useLayoutEffect(() => {
    dirtyRef.current = dirty;
  }, [dirty]);
  // Mirror `confirmingDiscard` so the Escape listener can read "is the
  // banner already up?" without re-subscribing on every toggle
  // (re-subscribing would race with the keystroke that triggered it).
  const confirmingDiscardRef = useRef(false);
  useEffect(() => {
    confirmingDiscardRef.current = confirmingDiscard;
  }, [confirmingDiscard]);
  // When `dirty` flips false while the banner is up (e.g. Save succeeds
  // and the parent re-emits the canonical graph_json), auto-dismiss so a
  // fresh Escape doesn't strand the user behind a stale discard prompt
  // for content they just persisted.
  useEffect(() => {
    if (!dirty) setConfirmingDiscard(false);
  }, [dirty]);

  const cancelButtonRef = useRef<HTMLButtonElement>(null);
  // WAI-ARIA APG alertdialog: when the banner appears, move focus to the
  // safer action so keyboard users can hit Space/Enter to dismiss without
  // reaching for the mouse.
  useEffect(() => {
    if (confirmingDiscard) cancelButtonRef.current?.focus();
  }, [confirmingDiscard]);

  const handleCancelDiscard = () => setConfirmingDiscard(false);
  const handleConfirmDiscard = () => {
    setConfirmingDiscard(false);
    onClose();
  };
  // ✕ click routes through the same guard as Escape (mirrors Modal's
  // `requestClose`, issue #808).
  const requestClose = () => {
    if (confirmingDiscard) return; // banner is up; its buttons decide
    if (dirty) {
      setConfirmingDiscard(true);
      return;
    }
    onClose();
  };

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

  // Esc closes (menus clear first via their own Escape handlers). When
  // the editor is dirty, route through the discard banner instead of
  // closing — matches `<Modal dirty>` (issue #1244).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      if (confirmingDiscardRef.current) {
        // Banner is up — Escape dismisses the prompt (Keep editing), it
        // must NOT confirm the discard. Reflexively pressing Escape to
        // dismiss a confirmation is the universal OS gesture, and it
        // maps to the safe option.
        setConfirmingDiscard(false);
        return;
      }
      if (dirtyRef.current) {
        setConfirmingDiscard(true);
        return;
      }
      onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

  return (
    <div
      className="absolute inset-0 z-40 flex flex-col bg-bg-base"
      data-testid="circuit-flow-editor"
    >
      {confirmingDiscard && (
        <div
          role="alertdialog"
          aria-labelledby="editor-discard-title"
          data-testid="editor-discard-banner"
          className="m-4 mb-0 border border-status-warning/40 bg-status-warning/10 rounded-md px-4 py-3 text-status-warning shrink-0"
        >
          <p id="editor-discard-title" className="text-base mb-2">
            Discard unsaved changes?
          </p>
          <div className="flex gap-2 justify-end">
            <button
              ref={cancelButtonRef}
              type="button"
              onClick={handleCancelDiscard}
              data-testid="editor-discard-cancel"
              className="px-3 py-1 text-sm rounded-md bg-bg-card text-text-secondary hover:bg-border-subtle"
            >
              Keep editing
            </button>
            <button
              type="button"
              onClick={handleConfirmDiscard}
              data-testid="editor-discard-confirm"
              className="px-3 py-1 text-sm rounded-md bg-status-error text-white hover:bg-status-error/90"
            >
              Discard changes
            </button>
          </div>
        </div>
      )}
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
          onClick={requestClose}
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
              position={quickConnect.screen}
              onSelect={(spec) => {
                addWiredNode(
                  spec.discriminator,
                  quickConnect.fromNodeId,
                  quickConnect.fromHandleId,
                  { x: quickConnect.flow.x - 110, y: quickConnect.flow.y - 32 }
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
          graph={currentGraph}
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
