import { describe, expect, it } from 'vitest';
import { backgroundInferenceOption } from '../../src/lib/backgroundInference';
import { HARNESS_CAPABILITIES } from '../../src/types/generated/HarnessCapabilitiesTable';
import type { SpawnOption } from '../../src/lib/groups';

function option(overrides: Partial<SpawnOption> = {}): SpawnOption {
  return {
    id: 'codex', label: 'Codex', icon: 'X', color: '', harness_id: 'codex',
    provider_id: null, is_proxied: false, group_key: 'codex',
    capabilities: HARNESS_CAPABILITIES.codex, ...overrides,
  };
}

describe('background settings choices', () => {
  it('rejects foreign runtimes on bare profiles and unresolved saved configurations', () => {
    for (const runtime of ['wsl', 'windowsinterop'] as const) {
      expect(backgroundInferenceOption(option({ runtime })).unavailable_reason)
        .toBe('background inference requires a host-native configuration');
      expect(backgroundInferenceOption(option({ runtime, configuration: {
        id: 'launch/foreign', name: 'Foreign', spawn_option_id: 'codex',
        model: null, effort: null, extra_args: null,
      } })).unavailable_reason).toBe('background inference requires a host-native configuration');
    }
  });
  it('uses the adapter capability for custom harness profiles', () => {
    const native = option({ id: 'my-codex', harness_id: 'my-codex' });
    expect(backgroundInferenceOption(native)).toBe(native);
    expect(backgroundInferenceOption(option({ capabilities: HARNESS_CAPABILITIES.freebuff })).unavailable_reason)
      .toBe('no one-shot background inference support');
  });

  it('requires declared provider routing and preserves existing unavailable reasons', () => {
    expect(backgroundInferenceOption(option({ is_proxied: true })).unavailable_reason)
      .toBe('background inference requires native authentication');
    const claude = option({ is_proxied: true, capabilities: HARNESS_CAPABILITIES.anthropic });
    expect(backgroundInferenceOption(claude)).toBe(claude);
    const unavailable = option({ unavailable_reason: 'provider key is missing' });
    expect(backgroundInferenceOption(unavailable)).toBe(unavailable);
  });

  it('refuses saved configurations with arguments that can change the background protocol', () => {
    const configured = option({ configuration: {
      id: 'launch/custom', name: 'Custom', spawn_option_id: 'codex', model: null,
      effort: null, extra_args: '--json',
    } });
    expect(backgroundInferenceOption(configured).unavailable_reason)
      .toBe('remove extra CLI arguments for background inference');
  });
});
