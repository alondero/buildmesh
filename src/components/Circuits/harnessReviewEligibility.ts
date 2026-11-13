/**
 * Review-circuit eligibility — whether a harness may start a review circuit.
 *
 * Split out of `harnessCapabilities.ts` (issue #2757, bundle size). The
 * predicate reads the *narrow* generated artifact
 * (`src/types/generated/HarnessReviewEligibility.ts`) rather than the full
 * capability catalogue, because the surfaces that ask this question — the
 * Agent Node review button, the Spawn Menu, the mobile review screen — are
 * rendered on first paint. Importing the full catalogue from there put a
 * 25 kB generated table into the eager entry chunk for four booleans.
 *
 * The full catalogue stays in `harnessCapabilities.ts`, which only the
 * Circuits Inspector (a lazy surface) reads.
 */
import {
  HARNESS_IDS,
  HARNESS_REVIEW_ELIGIBILITY,
  type InspectorHarnessId,
} from '../../types/generated/HarnessReviewEligibility';

/**
 * Resolve a stored provider id to the harness whose eligibility governs it,
 * or `null` when it names something this module doesn't model.
 *
 * `agent_nodes.provider` is an opaque string (`AgentNode.provider` in
 * `src/types/generated/AgentNode.ts`): a canonical harness id, a Spawn Option
 * id (`claude:minimax`), one of the legacy ids the column has carried since
 * before the harness/provider split, or a user-defined harness profile id.
 * The alias set below matches what the rest of the frontend already accepts —
 * `harnessIdFromProvider` in `Circuits/InspectorPanel.tsx` keeps the same
 * legacy mapping for the Inspector's provider field — plus the documented
 * `""` → `anthropic` rule the spawn resolver applies.
 *
 * The membership test is against the id list, not an object key: a key test
 * also matches `Object.prototype` names, so a provider string like
 * `"toString"` would resolve to a harness that does not exist and block a
 * review that Buildmesh cannot judge.
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
      return (HARNESS_IDS as readonly string[]).includes(normalised)
        ? (normalised as InspectorHarnessId)
        : null;
  }
}

/**
 * Whether a review circuit must not be started with this provider — as the
 * reviewed source agent or as the reviewer.
 *
 * `true` only for a harness Buildmesh can *prove* cannot yield a turn: the
 * plain shell, and a harness carrying no native attention hook, no passive
 * turn watcher and no result-file handoff. Every shipped agent harness can
 * write the result file a circuit prompt asks for, so a harness without a
 * lifecycle of its own (Freebuff, Perch, DeepSeek Harness) still completes
 * through that file. This is the harness half of the backend's Circuit
 * compatibility gate (`circuit::compatibility::harness_has_turn_signal`).
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
  const caps = HARNESS_REVIEW_ELIGIBILITY[harnessId];
  return (
    caps.is_plain_terminal
    || (!caps.requires_attention_hook
      && !caps.supports_passive_turn_watcher
      && !caps.supports_result_file_handoff)
  );
}
