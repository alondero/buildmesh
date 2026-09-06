/**
 * Exit-guard pure helpers (issue #1501).
 *
 * Single source of truth for the window-close confirmation flow:
 * which agent nodes count as "active", how to tell resumable from
 * non-resumable, and when a close request needs a prompt.
 *
 * Kept pure (no store / IPC reads) so the contract is unit-testable at
 * the `exit-guard.test.ts` seam — the `WindowCloseGuard` component owns
 * the Tauri `onCloseRequested` wiring and feeds these helpers.
 */

import type { AgentNode } from '../types/generated/AgentNode';
import type { ProviderInfo } from '../types/generated/ProviderInfo';
import type { SessionStatus } from '../types/generated/SessionStatus';

/** Node statuses that count as "active" for the exit prompt (issue #1501). */
export const ACTIVE_EXIT_STATUSES = [
  'running',
  'awaiting_input',
  'spawning',
  'ready',
] as const;

export type ExitActiveStatus = (typeof ACTIVE_EXIT_STATUSES)[number];

const ACTIVE_EXIT_SET: ReadonlySet<SessionStatus> = new Set<SessionStatus>(
  ACTIVE_EXIT_STATUSES as unknown as SessionStatus[],
);

/** True when the status counts as active for the exit confirmation. */
export function isActiveForExit(status: SessionStatus): boolean {
  return ACTIVE_EXIT_SET.has(status);
}

/** Filter to the nodes that block an immediate exit. */
export function getActiveExitNodes<T extends Pick<AgentNode, 'status'>>(
  nodes: readonly T[],
): T[] {
  return nodes.filter((n) => isActiveForExit(n.status));
}

/**
 * Executor (harness) half of a stored `AgentNode.provider` spawn-option id.
 * Splits composite `<harness>:<provider>` ids on the first `:` (same rule
 * as the backend `parse_spawn_option_id`). Empty string is the legacy
 * Anthropic default (see `AgentNode.provider` docs) — normalise to
 * `"anthropic"` so the resume lookup doesn't miss it.
 */
export function parseExitHarnessId(provider: string): string {
  const trimmed = provider.trim();
  if (trimmed === '') return 'anthropic';
  const colon = trimmed.indexOf(':');
  return colon === -1 ? trimmed : trimmed.slice(0, colon);
}

/**
 * Project a `ProviderInfo[]` into `harness_id → supports_resume`.
 * Prefers the native (non-proxied) row per harness — proxied children
 * share the harness executor, but the native row is the authoritative
 * descriptor. Falls back to the first row seen for a harness with no
 * native row.
 */
export function buildSupportsResumeMap(
  providers: readonly ProviderInfo[],
): Map<string, boolean> {
  const native = new Map<string, boolean>();
  const fallback = new Map<string, boolean>();
  for (const p of providers) {
    const supports = p.capabilities?.supports_resume ?? false;
    if (!fallback.has(p.harness_id)) fallback.set(p.harness_id, supports);
    if (!p.is_proxied && !native.has(p.harness_id)) {
      native.set(p.harness_id, supports);
    }
  }
  // Native wins where present, otherwise the first row's value.
  const merged = new Map<string, boolean>(fallback);
  for (const [k, v] of native) merged.set(k, v);
  return merged;
}

/**
 * True when an active node is expected to resume after a graceful exit:
 * it has a captured `cli_session_id` AND its harness supports resume.
 * Unknown harnesses are fail-closed (non-resumable) so the modal warns
 * rather than silently dropping work. Mirrors the issue #1501 rule:
 * `cli_session_id == null` or a non-resumable harness (e.g. `terminal`)
 * → non-resumable.
 *
 * The empty-string provider (the legacy Anthropic default — see
 * `AgentNode.provider` docs) normalises to `"anthropic"`, but the live
 * spawn menu may key the same executor as `"claude"` (post-#538 unified
 * harness id, both in `BUILTIN_HARNESS_IDS`). Check the alias twin before
 * giving up so legacy rows don't over-warn.
 */
export function isExitNodeResumable(
  node: Pick<AgentNode, 'provider' | 'cli_session_id'>,
  supportsResumeByHarness: ReadonlyMap<string, boolean>,
): boolean {
  const sessionId = node.cli_session_id;
  if (typeof sessionId !== 'string' || sessionId.length === 0) return false;
  const harnessId = parseExitHarnessId(node.provider);
  const direct = supportsResumeByHarness.get(harnessId);
  if (direct === true) return true;
  if (direct === false) return false;
  if (harnessId === 'anthropic') return supportsResumeByHarness.get('claude') === true;
  if (harnessId === 'claude') return supportsResumeByHarness.get('anthropic') === true;
  return false;
}

/** Split active nodes into resumable vs non-resumable (will-lose-work). */
export function partitionExitNodes<T extends Pick<AgentNode, 'provider' | 'cli_session_id'>>(
  activeNodes: readonly T[],
  supportsResumeByHarness: ReadonlyMap<string, boolean>,
): { resumable: T[]; nonResumable: T[] } {
  const resumable: T[] = [];
  const nonResumable: T[] = [];
  for (const n of activeNodes) {
    if (isExitNodeResumable(n, supportsResumeByHarness)) resumable.push(n);
    else nonResumable.push(n);
  }
  return { resumable, nonResumable };
}

/**
 * True when a close request must surface the confirmation modal:
 * at least one active node AND the user hasn't disabled the prompt.
 */
export function shouldConfirmExit<T extends Pick<AgentNode, 'status'>>(
  activeNodes: readonly T[],
  confirmBeforeQuit: boolean,
): boolean {
  return confirmBeforeQuit && activeNodes.length > 0;
}

/** Spec copy for the modal body (issue #1501 §3). */
export function formatExitBody(activeCount: number): string {
  return `You have ${activeCount} active agent session(s) running.`;
}

/**
 * Display provider for the `Node Name (Provider)` warning rows.
 * Prefers the friendly `ProviderInfo.label` for the exact spawn-option id,
 * then the harness row label, then the raw stored id (never blank).
 */
export function exitNodeProviderDisplay(
  node: Pick<AgentNode, 'provider'>,
  providers: readonly Pick<ProviderInfo, 'id' | 'harness_id' | 'label'>[],
): string {
  const raw = node.provider?.trim() !== '' ? node.provider : 'anthropic';
  const exact = providers.find((p) => p.id === raw);
  if (exact) return exact.label;
  const harnessId = parseExitHarnessId(raw);
  const byHarness = providers.find((p) => p.harness_id === harnessId);
  if (byHarness) return byHarness.label;
  return raw;
}
