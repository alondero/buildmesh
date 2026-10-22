/**
 * The Spawn Menu, flattened for the phone (issue #2385).
 *
 * The backend already derives one ordered Spawn Menu (`GET /api/providers`,
 * see `agent::provider_menu::available_providers`) and ADR-0016 forbids a
 * client re-sorting or re-deriving it: the desktop groups the rows by
 * harness, the phone renders them flat. The phone therefore does not build
 * its own list of harnesses; it flattens the backend rows into the two things
 * the spawn routes need:
 *
 *   * a bare **Spawn Option** row — `id` is the spawn option id, and the row
 *     launches itself;
 *   * a saved **Launch Configuration** row — the backend gives the row the
 *     configuration's own id and carries the configuration on the row, so it
 *     launches the row's *own* spawn option with that recipe beside it. The
 *     route pair is (`spawn_option_id`, `configuration_id`), which is the
 *     same pair the Android client sends.
 *
 * A launch that resolves the mesh's own default chain sends neither field.
 */

import type { Mesh, Provider } from "./api";

/**
 * The label for a launch that resolves the mesh's own default.
 *
 * The app-wide default is invisible to the phone, so an unpinned mesh reads
 * **Mesh default** — the same wording the Android client uses for that case.
 */
export const MESH_DEFAULT_LABEL = "Mesh default";

/**
 * One row of the Spawn Menu, resolved into the ids a launch request carries.
 */
export type LaunchChoice = {
  /** The menu row's own id — a Spawn Option id, or a saved configuration id. */
  id: string;
  /** The label the menu shows for the row. */
  label: string;
  /** The Spawn Option to run: `<harness>` or `<harness>:<provider>`. */
  provider: string;
  /** The saved Launch Configuration id, when the row is one. */
  configurationId?: string;
};

/**
 * The backend menu rows the phone offers for a launch.
 *
 * Only rows that can take a first prompt are offered. Every launch the phone
 * starts carries a prompt — the backend renders it from the issue or pull
 * request, and Capture writes it by hand — so a row that cannot prefill would
 * silently lose it. A row is also dropped when the backend reports it
 * unavailable, and a proxied row is kept only when it carries a saved
 * configuration, because a proxy without one has no provider to route to.
 *
 * The backend's order is preserved: the rows arrive grouped and ranked, and
 * the phone renders them in that order rather than sorting its own way.
 */
export function launchChoices(providers: Provider[]): LaunchChoice[] {
  return providers
    .filter((row) => !row.unavailable_reason)
    .filter(
      (row) =>
        row.capabilities?.supports_prefill &&
        (!row.is_proxied || row.configuration),
    )
    .map((row) => {
      const configuration = row.configuration;
      return {
        id: row.id,
        label: configuration?.name ?? row.label,
        provider: configuration?.spawn_option_id ?? row.id,
        ...(configuration ? { configurationId: configuration.id } : {}),
      };
    });
}

/**
 * The launch fields a spawn route expects, or an empty object for the mesh
 * default — which is what tells the backend to resolve its own default chain.
 */
export function launchFields(choice: LaunchChoice | null | undefined): {
  provider?: string;
  configuration_id?: string;
} {
  if (!choice) return {};
  return {
    provider: choice.provider,
    ...(choice.configurationId
      ? { configuration_id: choice.configurationId }
      : {}),
  };
}

/**
 * The label for a mesh's default launch.
 *
 * A mesh may pin its own default Spawn Option; the phone names it so the
 * split button's main half says what will actually run. An app-wide default
 * is invisible to the phone, so an unpinned mesh reads **Mesh default** —
 * the same wording the Android client uses for that case.
 */
export function meshDefaultLabel(
  mesh: Mesh | null | undefined,
  providers: Provider[],
): string {
  const pinned = mesh?.default_provider;
  if (!pinned) return MESH_DEFAULT_LABEL;
  const row = providers.find(
    (candidate) => candidate.id === pinned && !candidate.configuration,
  );
  return row?.label ?? MESH_DEFAULT_LABEL;
}
