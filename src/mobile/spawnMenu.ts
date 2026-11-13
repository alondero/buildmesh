/** Phone adapters for the backend Spawn Menu: ordered harness/login groups,
 * configuration compatibility and the fields carried by launch requests. */
import type { Mesh, Provider } from "./api";
import { offeredUnderLogin } from "../lib/launchSelection";

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
  harnessId?: string;
  harnessLabel?: string;
  loginId?: string;
  loginLabel?: string;
  unavailableReason?: string;
  warning?: string;
};

/** Preserve server order and filter by the launch context's prompt requirement.
 * Unavailable rows can remain visible with an explanation in selection controls. */
export function launchChoices(providers: Provider[], { needsPrompt = true, includeUnavailable = false }: {
  needsPrompt?: boolean; includeUnavailable?: boolean;
} = {}): LaunchChoice[] {
  return providers
    .filter((row) => includeUnavailable || !row.unavailable_reason)
    .filter((row) => (!needsPrompt || row.capabilities?.accepts_initial_prompt) && (!row.is_proxied || row.configuration))
    .flatMap((row) => {
      const configuration = row.configuration;
      const native = providers.find((p) => p.harness_id === row.harness_id && !p.configuration && !p.is_proxied);
      const base: LaunchChoice = {
        id: row.id,
        harnessId: row.group_key ?? row.harness_id,
        harnessLabel: native?.label ?? row.harness_id,
        label: configuration?.name ?? row.label,
        provider: configuration?.spawn_option_id ?? row.id,
        ...(configuration ? { configurationId: configuration.id } : {}),
        ...(row.unavailable_reason ? { unavailableReason: row.unavailable_reason } : {}),
        ...(row.verification_warning ? { warning: row.verification_warning } : {}),
      };
      const logins = native?.logins ?? [];
      if (logins.length < 2) return [base];
      return logins.filter((login) => !configuration || offeredUnderLogin(configuration, login.id))
        .map((login) => ({ ...base,
          id: loginChoiceId(row.id, login.id),
          label: `${base.label} · ${login.label}`,
          loginId: login.id, loginLabel: login.label,
          ...(!login.is_default && !login.signed_in ? { unavailableReason: 'Not signed in. Sign in from desktop Settings.' } : {}),
        }));
    });
}


/** JSON tuple encoding keeps ids distinct even when names contain separators. */
export function loginChoiceId(rowId: string, loginId: string): string {
  return JSON.stringify([rowId, loginId]);
}

/** Group in server order; web select controls and sheets consume the same rows. */
export function launchGroups(choices: LaunchChoice[]): { id: string; label: string; choices: LaunchChoice[] }[] {
  const groups = new Map<string, { id: string; label: string; choices: LaunchChoice[] }>();
  for (const choice of choices) {
    const id = JSON.stringify([choice.harnessId ?? choice.provider, choice.loginId]);
    const label = `${choice.harnessLabel ?? choice.provider}${choice.loginLabel ? ` · ${choice.loginLabel}` : ''}`;
    const group = groups.get(id) ?? { id, label, choices: [] };
    group.choices.push(choice);
    groups.set(id, group);
  }
  return [...groups.values()];
}

/**
 * The launch fields a spawn route expects, or an empty object for the mesh
 * default — which is what tells the backend to resolve its own default chain.
 */
export function launchFields(choice: LaunchChoice | null | undefined): {
  provider?: string;
  configuration_id?: string;
  login_id?: string;
} {
  if (!choice) return {};
  return {
    provider: choice.provider,
    ...(choice.loginId ? { login_id: choice.loginId } : {}),
    ...(choice.configurationId
      ? { configuration_id: choice.configurationId }
      : {}),
  };
}

/**
 * The mesh's pinned default Spawn Option, when the phone can resolve one.
 *
 * The desktop's spawn cluster draws the resolved default harness's brand mark
 * on its quick-spawn half (issue #2040). The phone can only resolve a
 * *pinned* mesh default: an app-wide default is invisible to the phone, so an
 * unpinned mesh has no row to draw a mark from and keeps the **Mesh default**
 * label alone.
 *
 * A saved-configuration row is not a default: `mesh.default_provider` names
 * a Spawn Option, and a saved row carries its configuration's own id, so it
 * does not match. The `!candidate.configuration` guard is the second net.
 */
export function meshDefaultProvider(
  mesh: Mesh | null | undefined,
  providers: Provider[],
): Provider | null {
  const pinned = mesh?.default_provider;
  if (!pinned) return null;
  return (
    providers.find(
      (candidate) => candidate.id === pinned && !candidate.configuration,
    ) ?? null
  );
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
  return meshDefaultProvider(mesh, providers)?.label ?? MESH_DEFAULT_LABEL;
}
