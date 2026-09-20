/**
 * Frontend helpers over the generated harness catalog — the Inspector's
 * lookup/effort logic, plus the circuit-eligibility predicate shared by every
 * surface that spawns or waits on an agent.
 *
 * Capability values and labels live in
 * `src/types/generated/HarnessCapabilitiesTable.ts` (emitted from
 * `src-tauri/src/agent/harness_catalog.rs` by `cargo test`). This module
 * keeps lookup, effort-vocabulary, and eligibility logic only — a
 * hand-written per-harness table here is a defect (ADR-0037).
 */

import type { HarnessCapabilities } from '../../types/generated/HarnessCapabilities';
import {
  HARNESS_CAPABILITIES,
  HARNESS_PROFILE_ALIASES,
  type InspectorHarnessId,
} from '../../types/generated/HarnessCapabilitiesTable';

export {
  HARNESS_CAPABILITIES,
  HARNESS_IDS,
  HARNESS_LABEL,
  HARNESS_PROFILE_ALIASES,
  type InspectorHarnessId,
} from '../../types/generated/HarnessCapabilitiesTable';

/**
 * Look up the capability descriptor for a harness id. `null` provider and
 * unknown ids return `null` so the Inspector renders the "no overrides"
 * fallback. The profile id `claude` maps to the `anthropic` adapter via
 * `HARNESS_PROFILE_ALIASES`.
 */
export function getCapabilitiesFor(
  harnessId: string | null | undefined,
): HarnessCapabilities | null {
  if (!harnessId) return null;
  const canonical = HARNESS_PROFILE_ALIASES[harnessId] ?? harnessId;
  if (canonical in HARNESS_CAPABILITIES) {
    return HARNESS_CAPABILITIES[canonical as InspectorHarnessId];
  }
  return null;
}

/**
 * The full vocabulary for a given harness's effort select, used by the
 * Inspector's `<select>` children. Returns `[]` for harnesses with no
 * effort control so the dropdown renders the empty state.
 */
export function effortAllowedFor(caps: HarnessCapabilities): string[] {
  const ctl = caps.effort_control;
  if (ctl.kind === 'none') return [];
  return ctl.allowed;
}

/**
 * Resolve a stored provider id to the harness whose capability descriptor
 * governs it, or `null` when it names something this module doesn't model.
 *
 * `agent_nodes.provider` is an opaque string (`AgentNode.provider` in
 * `src/types/generated/AgentNode.ts`): a canonical harness id, a Spawn Option
 * id (`claude:minimax`), one of the legacy ids the column has carried since
 * before the harness/provider split, or a user-defined harness profile id.
 * The alias set below matches what the rest of the frontend already accepts —
 * `harnessIdFromProvider` in `Circuits/InspectorPanel.tsx` keeps the same
 * legacy mapping for the Inspector's provider field — plus the documented
 * `""` → `anthropic` rule the spawn resolver applies.
 */
function harnessIdForProvider(
  providerId: string | null | undefined,
): InspectorHarnessId | null {
  if (providerId == null) return null;
  const separator = providerId.indexOf(':');
  const harnessHalf = separator === -1 ? providerId : providerId.slice(0, separator);
  const normalised = harnessHalf.trim().toLowerCase();
  switch (normalised) {
    // Empty is documented as "anthropic" for this column, so an unset value
    // must not read as an unmodelled harness.
    case '':
    case 'claude':
    case 'claude_code':
    case 'anthropic':
      return 'anthropic';
    case 'antigravity':
      return 'agy';
    case 'minimax-code':
      return 'mcode';
    case 'deepseek':
    case 'deepseek-harness':
      return 'dsh';
    case 'command-code':
    case 'cmdc':
    case 'cmd':
      return 'commandcode';
    default:
      return normalised in HARNESS_CAPABILITIES
        ? (normalised as InspectorHarnessId)
        : null;
  }
}

/**
 * Whether a review circuit must not be started with this provider — as the
 * reviewed source agent or as the reviewer.
 *
 * `true` only for a harness Buildmesh can *prove* cannot yield a turn: the
 * ones carrying neither a native attention hook nor a passive turn watcher,
 * plus the plain shell. This is the harness half of the backend's Autopilot
 * compatibility gate (`autopilot::compatibility::evaluate`). A circuit gate
 * that waits on an agent (`AwaitAgentTurn`, `ReviewVerdict`) only advances
 * once that agent's status reaches `AwaitingInput`/`Ready`/`Completed`, and
 * those statuses arrive only from the attention/lifecycle path — so such a run
 * does not fail, it parks until the watchdog budget expires.
 *
 * An id that resolves to nothing returns `false`. `AgentNode.provider` holds
 * user-defined harness profile ids, which the spawn seam resolves to a real
 * executor; refusing those would take a working review away from the user, so
 * this gate only blocks what it can positively judge.
 */
export function blocksReviewCircuit(
  providerId: string | null | undefined,
): boolean {
  const harnessId = harnessIdForProvider(providerId);
  if (harnessId === null) return false;
  const caps = HARNESS_CAPABILITIES[harnessId];
  return (
    caps.is_plain_terminal
    || (!caps.requires_attention_hook && !caps.supports_passive_turn_watcher)
  );
}
