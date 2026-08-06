import { describe, it, expect } from 'vitest';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';
import {
  groupByHarness,
  hasSpawnableAgent,
  mapBackendProviders,
  colorClassForProvider,
  type SpawnOption,
} from '../../src/lib/groups';

// Issue #583 cleanup — `groupByHarness`, `mapBackendProviders`, and the
// `SpawnOption` projection are the canonical helpers for the
// harness-grouped Spawn Menu (issue #575 / ADR-0016). They replaced
// three inline bucketing sites (GroupedProviderMenu, MeshPropertiesTab,
// mobile ProviderPicker) and four inline 8-field projections
// (Sidebar, ArchivedNodesTab, GitIssuesTab, GitPullRequestsTab). The
// tests below pin the contracts those call sites depend on.

function row(
  id: string,
  harness_id: string,
  is_proxied: boolean,
  group_key = harness_id,
  // Use `||` (not `??`) so an empty provider half like `id = 'claude:'`
  // resolves to `null` (an empty string is a valid `string` but not a
  // valid `provider_id`). The fixture is only test-side, but a
  // malformed id that slipped past `??` would silently break the
  // projection test without failing the fixture build.
  provider_id: string | null = is_proxied ? id.split(':')[1] || null : null,
): ProviderInfo {
  return {
    id,
    label: id,
    color: '#000000',
    icon: 'X',
    resumable: false,
    harness_id,
    provider_id,
    is_proxied,
    group_key,
  };
}

describe('groupByHarness', () => {
  it('buckets a flat Spawn Option list by group_key in input order', () => {
    const providers = [
      row('claude', 'claude', false),
      row('claude:minimax', 'claude', true),
      row('codex', 'codex', false),
      row('terminal', 'terminal', false),
    ];
    const groups = groupByHarness(providers);
    expect(groups.map(([key]) => key)).toEqual([
      'claude',
      'codex',
      'terminal',
    ]);
    // Within each bucket, input order is preserved so the native row
    // lands first (it's the clickable header).
    expect(groups[0][1].map((p) => p.id)).toEqual(['claude', 'claude:minimax']);
    expect(groups[1][1].map((p) => p.id)).toEqual(['codex']);
    expect(groups[2][1].map((p) => p.id)).toEqual(['terminal']);
  });

  it('returns an empty array for an empty input', () => {
    expect(groupByHarness([])).toEqual([]);
  });

  it('drops filtered-out rows before bucketing (a filtered harness header collapses the bucket)', () => {
    // Code-review finding B2 from issue #575 — a filter that removes the
    // native row must collapse the bucket to its children rather than
    // mislabeling a Proxied child as the harness header.
    const providers = [
      row('claude', 'claude', false),
      row('claude:minimax', 'claude', true),
      row('claude:kimi', 'claude', true),
      row('codex', 'codex', false),
    ];
    // Filter removes the native `claude` row.
    const groups = groupByHarness(providers, {
      filter: (p) => p.id !== 'claude',
    });
    expect(groups.map(([key, options]) => [key, options.map((p) => p.id)])).toEqual([
      ['claude', ['claude:minimax', 'claude:kimi']],
      ['codex', ['codex']],
    ]);
  });

  it('uses the generic `group_key` constraint so the mobile Provider shape reuses the helper', () => {
    // Mobile's `Provider` carries `group_key` but not `harness_id` as a
    // top-level field. The generic `T extends { group_key: string }`
    // bound lets the same helper bucket mobile rows without a separate
    // projection.
    type MobileProvider = { group_key: string; id: string };
    const providers: MobileProvider[] = [
      { id: 'claude', group_key: 'claude' },
      { id: 'claude:mm', group_key: 'claude' },
    ];
    expect(groupByHarness(providers).map(([k, v]) => [k, v.map((p) => p.id)])).toEqual([
      ['claude', ['claude', 'claude:mm']],
    ]);
  });
});

describe('hasSpawnableAgent', () => {
  it('is false when the menu is empty (no meshes-style edge case)', () => {
    expect(hasSpawnableAgent([])).toBe(false);
  });

  it('is false when only the Terminal harness is present (fresh machine, issue #822)', () => {
    // No agent CLI detected and no keyed provider → backend emits only the
    // code-defined Terminal harness. This is the onboarding empty state.
    expect(hasSpawnableAgent([row('terminal', 'terminal', false)])).toBe(false);
  });

  it('is true when a native agent harness is present', () => {
    expect(
      hasSpawnableAgent([
        row('claude', 'claude', false),
        row('terminal', 'terminal', false),
      ]),
    ).toBe(true);
  });

  it('is true when a keyed Proxied provider is the only non-terminal option', () => {
    // A keyed account surfaces a Proxied row under a non-terminal harness id
    // even when the Claude binary is absent — so "provider keyed" counts as
    // spawnable and suppresses the empty state.
    expect(
      hasSpawnableAgent([
        row('claude:minimax', 'claude', true),
        row('terminal', 'terminal', false),
      ]),
    ).toBe(true);
  });
});

describe('mapBackendProviders', () => {
  it('projects ProviderInfo into the 8-field SpawnOption shape', () => {
    const backend: ProviderInfo[] = [
      row('claude', 'claude', false),
      row('claude:minimax', 'claude', true),
      row('terminal', 'terminal', false),
    ];
    const result = mapBackendProviders(backend);
    expect(result).toHaveLength(3);
    expect(result[0]).toEqual<SpawnOption>({
      id: 'claude',
      label: 'claude',
      icon: 'X',
      harness_id: 'claude',
      provider_id: null,
      is_proxied: false,
      group_key: 'claude',
      color: colorClassForProvider('claude'),
    });
    expect(result[1]).toMatchObject({
      id: 'claude:minimax',
      is_proxied: true,
      provider_id: 'minimax',
      color: 'bg-indigo-500',
    });
  });

  it('drops backend-only metadata (resumable, hex color) from the projected shape', () => {
    // The projection is the frontend's view — backend metadata that no
    // spawn surface reads (e.g. `resumable`) must NOT bleed into the
    // shared `SpawnOption` type, else every consumer carries a field it
    // never reads. The ArchivedNodesTab caller merges `resumable` back
    // in via its own post-projection step.
    const backend: ProviderInfo[] = [row('claude', 'claude', false)];
    const result = mapBackendProviders(backend);
    expect(result[0]).not.toHaveProperty('resumable');
    // `color` is replaced with the Tailwind class — backend's hex is
    // dropped.
    expect(result[0].color).not.toMatch(/^#/);
    expect(result[0].color).toMatch(/^bg-/);
  });

  it('preserves input order (the harness-grouped render relies on it)', () => {
    const backend: ProviderInfo[] = [
      row('claude', 'claude', false),
      row('codex', 'codex', false),
      row('terminal', 'terminal', false),
    ];
    expect(mapBackendProviders(backend).map((p) => p.id)).toEqual([
      'claude',
      'codex',
      'terminal',
    ]);
  });
});

describe('colorClassForProvider', () => {
  it('maps known harness ids to Tailwind badge classes', () => {
    expect(colorClassForProvider('claude')).toBe('bg-blue-500');
    expect(colorClassForProvider('minimax')).toBe('bg-indigo-500');
    expect(colorClassForProvider('kimi')).toBe('bg-cyan-500');
    expect(colorClassForProvider('agy')).toBe('bg-emerald-500');
    expect(colorClassForProvider('opencode')).toBe('bg-amber-500');
    expect(colorClassForProvider('terminal')).toBe('bg-gray-500');
  });

  it('falls back to bg-gray-500 for unknown ids (mirrors the legacy behavior)', () => {
    expect(colorClassForProvider('mystery-harness')).toBe('bg-gray-500');
    expect(colorClassForProvider('')).toBe('bg-gray-500');
  });
});
