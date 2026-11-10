import type { RegenerateSelection } from '../regenerate';
import { subscribeRawPtyOutput, unsubscribeRawPtyOutput } from './ptyOutput';
export { bytesFromChannelMessage } from './ptyOutput';
import { _invoke } from './_invoke';
import { TerminalInputQueue, type InputStall } from '../terminalInputQueue';
import type { InputOutcome } from '../../types/generated/InputOutcome';
import type { AgentNode } from '../../types/generated/AgentNode';
import type { SpawnAgentRequest } from '../../types/generated/SpawnAgentRequest';

// Agent
export const spawnAgent = (request: SpawnAgentRequest) =>
  _invoke('spawn_agent', { request });

// Issue #774 / #775 — swap a node's Model Provider. The worktree,
// branch, name, and position are preserved; only `provider` changes. The
// backend decides resume vs fresh from the new provider's harness.
export const regenerateAgentNode = (nodeId: number, newProviderId: string, selection?: RegenerateSelection) =>
  _invoke<AgentNode>('regenerate_agent_node', { nodeId, newProviderId, ...selection });

export const killAgent = (sessionId: number) =>
  _invoke('kill_agent', { sessionId });

export const isAgentRunning = (sessionId: number) =>
  _invoke<boolean>('is_agent_running', { sessionId });

export const sendToAgent = (sessionId: number, input: string) =>
  _invoke('send_to_agent', { sessionId, input });

/** Raw write to the agent's PTY (no submit/newline handling — cf. `sendToAgent`).
 *
 * Bypasses the ordered retry buffer on purpose: this is the transport
 * itself, used by the queue and by tests that need to observe a refused
 * write. Product callers use `writeToAgent` below.
 */
export const writeToAgentRaw = (sessionId: number, data: string) =>
  _invoke<InputOutcome>('write_to_agent', { sessionId, data });

const terminalInputQueue = new TerminalInputQueue({ write: writeToAgentRaw });

/**
 * Watch for a session whose input is being held rather than delivered.
 *
 * The transport owns the buffer, so the UI subscribes here instead of the
 * transport reaching into a store. Reports only once bytes have been held
 * past the queue's threshold, so a normal burst of typing shows nothing. The
 * `nodeId` scopes every report and every withdrawal to one session, so a
 * recovering agent cannot clear a badge another agent still needs.
 */
export const subscribeTerminalInputStall = (
  listener: (nodeId: number, stall: InputStall | null) => void,
) => terminalInputQueue.subscribeStall(listener);

/** Forget everything pending for a session (node closed, mesh torn down). */
export const cancelTerminalInput = (sessionId: number) => terminalInputQueue.cancel(sessionId);

/**
 * Write to the agent's PTY through the one ordered retry buffer (issue #1530).
 *
 * Every product caller uses this rather than the raw invoke, so no call site
 * can invent its own backpressure behaviour: the buffer preserves input
 * order per session, retries a refused write with the identical bytes (never
 * a duplicate — a refusal means the bytes were never queued), and settles
 * with the disposition that ultimately applied. A rejected IPC call still
 * rejects, so `pasteClipboard` can tell "could not try" from "tried and was
 * refused".
 */
export const writeToAgent = (sessionId: number, data: string) =>
  terminalInputQueue.enqueue(sessionId, data);

/**
 * Hand `text` over to an existing agent (`handover_to_agent`). Not a
 * `writeToAgent` convenience wrapper: the backend owns staging the text as one
 * bracketed paste and submitting it with a decoupled Enter, so a multi-line
 * handover cannot submit at every newline (issue #874).
 */
export const handoverToAgent = (targetNodeId: number, text: string) =>
  _invoke('handover_to_agent', { targetNodeId, text });

// Attention
export const registerAttentionNode = (nodeId: number) =>
  _invoke('register_attention_node', { nodeId });

export const clearAttentionNode = (nodeId: number) =>
  _invoke('clear_attention_node', { nodeId });

export const isAttentionPending = (nodeId: number) =>
  _invoke<boolean>('is_attention_pending', { nodeId });

export const spawnHandoverAgent = (meshId: number, prefill: string, provider?: string) =>
  _invoke<AgentNode>('spawn_handover_agent', { meshId, prefill, provider });

// ── Agent PTY transport ────────────────────────────────────────────────────
//
// `writeToAgent` is declared above next to the other agent IPCs; `resizeAgent`
// here completes the PTY-side surface used by `TerminalRegistry`. It rejects
// with the string `'Agent not running'` while the PTY isn't up yet — callers
// match on that exact value to ignore the expected race (see
// `TerminalRegistry.syncPtySize`).
export const resizeAgent = (sessionId: number, rows: number, cols: number) =>
  _invoke('resize_agent', { sessionId, rows, cols });

/**
 * Subscribe this webview to raw agent PTY bytes for `sessionId` (issue #1385).
 * Production output skips Base64+JSON.
 */
export const subscribeAgentOutput = (
  sessionId: number,
  onChunk: (data: Uint8Array) => void,
): Promise<void> => subscribeRawPtyOutput('subscribe_agent_output', sessionId, onChunk);

/** Drop the binary Channel registered by [`subscribeAgentOutput`]. Idempotent. */
export const unsubscribeAgentOutput = (sessionId: number, owner?: Promise<void>) =>
  unsubscribeRawPtyOutput('subscribe_agent_output', sessionId, owner);

/**
 * Subscribe this webview to raw Build/Run PTY bytes for `sessionId`
 * (issue #1393). Same Channel transport as [`subscribeAgentOutput`].
 */
export const subscribeBuildRunOutput = (
  sessionId: number,
  onChunk: (data: Uint8Array) => void,
): Promise<void> => subscribeRawPtyOutput('subscribe_build_run_output', sessionId, onChunk);

/** Drop the binary Channel registered by [`subscribeBuildRunOutput`]. Idempotent. */
export const unsubscribeBuildRunOutput = (sessionId: number, owner?: Promise<void>) =>
  unsubscribeRawPtyOutput('subscribe_build_run_output', sessionId, owner);

/** Reply to a remote-pane snapshot request from the HTTP server. The pair
 *  (`request_id`, `data`) is matched against an in-flight promise on the
 *  backend; the call returns immediately and has no result. */
export const submitTerminalSnapshot = (requestId: string, data: string) =>
  _invoke<void>('submit_terminal_snapshot', { requestId, data });

// ── Build/Run side-panel PTY ───────────────────────────────────────────────
//
// Separate PTY surface from the agent terminal — keeps a long-running build
// or `npm run dev` independent of the agent's PTY lifecycle. `build_run`
// returns once the child has been spawned; production output flows via
// `subscribeBuildRunOutput`. The `build-run-output-<nodeId>` event is the
// test-injection fallback.
export const buildRun = (nodeId: number, mode: 'build' | 'run' | 'terminal') =>
  _invoke('build_run', { nodeId, mode });

export const closeBuildRun = (nodeId: number) =>
  _invoke('close_build_run', { nodeId });

export const writeToBuildRun = (nodeId: number, data: string) =>
  _invoke('write_to_build_run', { nodeId, data });

export const resizeBuildRun = (nodeId: number, rows: number, cols: number) =>
  _invoke('resize_build_run', { nodeId, rows, cols });
