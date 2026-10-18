import { _invoke } from './_invoke';
import type { AutopilotCircuit } from '../../types/generated/AutopilotCircuit';
import type { CircuitAgentOwnership } from '../../types/generated/CircuitAgentOwnership';
import type { CircuitRunDetail } from '../../types/generated/CircuitRunDetail';
import type { CircuitWithRuns } from '../../types/generated/CircuitWithRuns';
import type { CircuitProbeSnapshot } from '../../types/generated/CircuitProbeSnapshot';
import type { CircuitQueueDirection } from '../../types/generated/CircuitQueueDirection';
import type { CircuitQueueEntry } from '../../types/generated/CircuitQueueEntry';
import type { CircuitTriggerKind } from '../../types/generated/CircuitTriggerKind';
import type { CircuitBlueprintKind } from '../../types/generated/CircuitBlueprintKind';

// Milestone 4 (#1209): the canvas editor consumes the blueprint AST, so
// the graph wire types ride the same sanctioned surface.
export type {
  AutopilotCircuit,
  CircuitAgentOwnership,
  CircuitQueueDirection,
  CircuitQueueEntry,
  CircuitRunDetail,
  CircuitWithRuns,
  CircuitProbeSnapshot,
};

export type { CircuitGraph } from '../../types/generated/CircuitGraph';

export type { CircuitNode } from '../../types/generated/CircuitNode';

export type { CircuitNodeKind } from '../../types/generated/CircuitNodeKind';

export type { CircuitBlueprintKind } from '../../types/generated/CircuitBlueprintKind';

export type { CircuitEdge } from '../../types/generated/CircuitEdge';

export type { EdgeCondition } from '../../types/generated/EdgeCondition';

export type { StepOutcome } from '../../types/generated/StepOutcome';

export type { RunState } from '../../types/generated/RunState';

export type { StepStatus } from '../../types/generated/StepStatus';

export type { CapacityBind } from '../../types/generated/CapacityBind';

export type { GithubActionKind } from '../../types/generated/GithubActionKind';

export type { OpenPrPolicy } from '../../types/generated/OpenPrPolicy';

export type { SessionStatusKind } from '../../types/generated/SessionStatusKind';

export const listCircuits = (meshId: number) =>
  _invoke<AutopilotCircuit[]>('list_circuits', { meshId });

export const listCircuitAgentOwnerships = () =>
  _invoke<CircuitAgentOwnership[]>('list_circuit_agent_ownerships');

/** One circuit row — the canvas editor overlay's load unit (#1209). */
export const getCircuit = (circuitId: number) =>
  _invoke<AutopilotCircuit>('get_circuit', { circuitId });

/** Batched single-IPC load for the Probe tab: every circuit on the mesh with
 *  all active runs plus up to `limit` newest terminal runs (steps included). */
export const listCircuitsWithRuns = (meshId: number, limit?: number) =>
  _invoke<CircuitWithRuns[]>('list_circuits_with_runs', { meshId, limit });

/** Single-IPC hydration for the Circuits Probe ledger and mesh queue. */
export const listCircuitProbe = (meshId: number, limit?: number) =>
  _invoke<CircuitProbeSnapshot>('list_circuit_probe', { meshId, limit });

export const listCircuitQueue = (meshId: number) =>
  _invoke<CircuitQueueEntry[]>('list_circuit_queue', { meshId });

/** Creates a circuit with the canonical server-side blueprint:
 *  <trigger> → SpawnAgentNode (fresh) → InjectPty(prompt) → Notify.
 *  Trigger vocabulary (issue #1208): manual (default), interval,
 *  github_issue_label, github_pr_label. */
export type { CircuitTriggerKind } from '../../types/generated/CircuitTriggerKind';

export const createCircuit = (
  meshId: number,
  name: string,
  description: string,
  initialPrompt: string,
  triggerKind: CircuitTriggerKind = 'manual',
  triggerLabel?: string,
  intervalSeconds?: number,
  blueprint: CircuitBlueprintKind = 'walking_skeleton'
) =>
  _invoke<AutopilotCircuit>('create_circuit', {
    meshId,
    name,
    description,
    initialPrompt,
    triggerKind,
    triggerLabel: triggerLabel ?? null,
    intervalSeconds: intervalSeconds ?? null,
    blueprint,
  });

export const setCircuitEnabled = (circuitId: number, enabled: boolean) =>
  _invoke<void>('set_circuit_enabled', { circuitId, enabled });

/** Canvas editor save seam (issue #1209): replace the whole blueprint
 *  AST. The backend validates the JSON parses before persisting. */
export const updateCircuitGraph = (circuitId: number, graphJson: string) =>
  _invoke<void>('update_circuit_graph', { circuitId, graphJson });

export const deleteCircuit = (circuitId: number) =>
  _invoke<void>('delete_circuit', { circuitId });

export const cancelCircuitRun = (runId: number) =>
  _invoke<void>('cancel_circuit_run', { runId });

export const moveCircuitRun = (runId: number, direction: CircuitQueueDirection) =>
  _invoke<void>('move_circuit_run', { runId, direction });

/** Persist an explicit front-to-back queue order (drag-drop / keyboard reorder). */
export const reorderCircuitQueue = (meshId: number, orderedRunIds: number[]) =>
  _invoke<void>('reorder_circuit_queue', { meshId, orderedRunIds });

/** Bulk cancel through the single-run cleanup path (one event per run). */
export const cancelCircuitRuns = (runIds: number[]) =>
  _invoke<void>('cancel_circuit_runs', { runIds });

export const triggerCircuitNow = (circuitId: number) =>
  _invoke<number>('trigger_circuit_now', { circuitId });

export const triggerCircuitFromNode = (
  nodeId: number,
  circuitId: number | null,
  maxRounds: number,
  reviewerProvider: string | null = null,
) =>
  _invoke<number>('trigger_circuit_from_node', { nodeId, circuitId, maxRounds, reviewerProvider });

/** Graceful pause: the graph stops advancing; current steps finish (#1207). */
export const pauseCircuitRun = (runId: number) =>
  _invoke<void>('pause_circuit_run', { runId });

/** Resume a paused run where it stopped (#1207). */
export const continueCircuitReview = (runId: number, additionalRounds = 1) =>
  _invoke<number>('continue_circuit_review', { runId, additionalRounds });

export const resumeCircuitRun = (runId: number) =>
  _invoke<void>('resume_circuit_run', { runId });

/** Approve a CollaboratorCheck gate parked in `blocked` (#1207). */
export const approveCircuitStep = (runId: number, nodeId: string) =>
  _invoke<void>('approve_circuit_step', { runId, nodeId });

export const listCircuitRuns = (circuitId: number, limit?: number) =>
  _invoke<CircuitRunDetail[]>('list_circuit_runs', { circuitId, limit });

/** Select a host-native Claude Code or native Codex configuration for Circuit classifiers. */
export const setCircuitClassifierProvider = (provider: string | null) =>
  _invoke<void>('set_circuit_classifier_provider', { provider });

export const listSemanticTurns = () =>
  _invoke<import('../../types/generated/SemanticTurnPayload').SemanticTurnPayload[]>('list_semantic_turns');
