/**
 * Inspector helpers over the generated harness catalog.
 *
 * Capability values and labels live in
 * `src/types/generated/HarnessCapabilitiesTable.ts` (emitted from
 * `src-tauri/src/agent/harness_catalog.rs` by `cargo test`). This module
 * keeps lookup and effort-vocabulary logic only — a hand-written per-harness
 * table here is a defect (ADR-0037).
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
