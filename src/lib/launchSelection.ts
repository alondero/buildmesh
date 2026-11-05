import type { SpawnConfiguration } from '../types/generated/SpawnConfiguration';

/** Unpinned recipes follow the selected login; provider routes use Default. */
export function offeredUnderLogin(configuration: SpawnConfiguration, loginId: string | undefined): boolean {
  if (loginId === undefined) return true;
  const routed = Boolean(configuration.provider_route_id) || configuration.spawn_option_id.includes(':');
  if (routed) return loginId === 'default';
  return !configuration.login_id || configuration.login_id === loginId;
}
