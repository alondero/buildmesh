import type { SpawnOption } from './groups';

/** Apply the adapter's background contract to native and saved settings choices. */
export function backgroundInferenceOption(option: SpawnOption): SpawnOption {
  if (option.unavailable_reason) return option;
  const capability = option.capabilities?.background_inference;
  const plan = option.configuration?.resolved;
  const runtime = option.runtime ?? plan?.harness.runtime;
  const routed = option.is_proxied || plan?.route || option.configuration?.provider_route_id
    || option.configuration?.spawn_option_id.includes(':');
  let reason: string | undefined;
  if (!capability) reason = 'no one-shot background inference support';
  else if (runtime === 'wsl' || runtime === 'windowsinterop') {
    reason = 'background inference requires a host-native configuration';
  } else if (routed && !capability.supports_provider_routing) {
    reason = 'background inference requires native authentication';
  } else if (option.configuration?.extra_args?.trim()) {
    reason = 'remove extra CLI arguments for background inference';
  }
  return reason ? { ...option, unavailable_reason: reason } : option;
}
