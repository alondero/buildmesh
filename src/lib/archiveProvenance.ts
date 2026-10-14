/**
 * Harness provenance for Archive entries (issue #2332).
 *
 * Discovery stamps each entry with the harness that owns the conversation
 * (`ArchivedAgentNode.provider`), and Resume has to hand that harness's
 * executor to the backend instead of guessing. Both surfaces — the desktop
 * Archive tab and the mobile Archive list — need the same three questions
 * answered, so they ask them here:
 *
 *   1. Which harness is this, for display?
 *   2. Can it resume at all?
 *   3. Which Spawn Option runs it?
 *
 * The vocabulary on both sides of this file differs on purpose. An archive
 * entry carries an *adapter id* (`anthropic`, `cursor`, `commandcode`,
 * `agy`) because that is what `agent_nodes.provider` and the resume argv are
 * built from. A Spawn Menu row carries a *Harness Profile id*
 * (`claude`, `agy`, `opencode`) and may be a Proxied Provider composite
 * (`claude:minimax`). `HARNESS_PROFILE_ALIASES` is the generated map between
 * them (`claude` → `anthropic`), generated from the Rust harness catalog, so
 * this file does not hand-copy the table.
 */
import { HARNESS_LABEL, HARNESS_PROFILE_ALIASES } from "../types/generated/HarnessCapabilitiesTable";

/** The shape this module needs from a Spawn Menu row. */
interface SpawnOptionLike {
  id: string;
  harness_id: string;
  is_proxied: boolean;
}

/**
 * Split a stored provider id into its Harness Profile id. A Proxied Provider
 * id is `<harness_id>:<provider_id>`; anything else is the harness id itself.
 * Mirrors `SpawnOptionId::from` on the Rust side.
 */
export const harnessIdFromProvider = (provider: string): string =>
  provider.includes(':') ? provider.slice(0, provider.indexOf(':')) : provider;

/**
 * The adapter id for a Harness Profile id (`claude` → `anthropic`). An
 * unrecognised id is returned unchanged so an id this build does not know
 * still round-trips rather than becoming `undefined`.
 */
export const adapterIdFromHarnessId = (harnessId: string): string =>
  HARNESS_PROFILE_ALIASES[harnessId] ?? harnessId;

/** The adapter id a stored provider id names (`claude:minimax` → `anthropic`). */
export const adapterIdFromProvider = (provider: string): string =>
  adapterIdFromHarnessId(harnessIdFromProvider(provider));

/**
 * A human label for an archive entry's provenance, or `null` when the
 * provenance is absent or unrecognised.
 *
 * `null` is not cosmetic: it is the case the backend refuses to resume
 * (#2332), so a surface must show an honest unavailable state rather than
 * falling back to "Claude Code".
 */
export const archiveHarnessLabel = (provider?: string | null): string | null => {
  const trimmed = provider?.trim();
  if (!trimmed) return null;
  const adapter = adapterIdFromProvider(trimmed);
  const label = HARNESS_LABEL[adapter as keyof typeof HARNESS_LABEL];
  // A Proxied Provider row has no single label ("claude:minimax"); naming the
  // harness it runs on is the honest answer.
  return label ?? harnessIdFromProvider(trimmed);
};

/**
 * The Spawn Option that runs an entry's own harness, or `undefined` when
 * that harness has no option here — either because it is not installed, or
 * because the entry carries no provenance.
 *
 * Only native (non-proxied) rows match: a proxied row is a Model Provider
 * choice *inside* a harness, so `claude:minimax` must not be picked as the
 * Cursor harness's executor. The first matching row wins, which is the
 * menu's own ordering.
 */
export const resumeOptionForHarness = <T extends SpawnOptionLike>(
  options: readonly T[],
  provider?: string | null,
): T | undefined => {
  const trimmed = provider?.trim();
  if (!trimmed) return undefined;
  const wanted = adapterIdFromProvider(trimmed);
  return options.find(
    (option) => !option.is_proxied && adapterIdFromHarnessId(option.harness_id) === wanted,
  );
};

/**
 * The executor id a Resume request should carry for an archive entry: the
 * entry's own harness when its Spawn Option is available, otherwise `null`.
 *
 * `null` means "this surface cannot resume it" — the caller must show the
 * unavailable state instead of substituting a default provider, because a
 * wrong executor resumes nothing (issue #2332).
 */
export const resumeProviderForHarness = <T extends SpawnOptionLike>(
  options: readonly T[],
  provider?: string | null,
): string | null => resumeOptionForHarness(options, provider)?.id ?? null;