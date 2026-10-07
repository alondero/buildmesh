/**
 * Shared vocabulary for the Harnesses pane's proxied-provider surface
 * (issue #1880).
 *
 * These three constants are read by both `HarnessCard` and
 * `ProxiedChildRow`. Keeping them here rather than in `HarnessCard` is what
 * lets `HarnessCard` render `ProxiedChildRow` without the two importing
 * each other — a value-bearing cycle between them would make module
 * evaluation order load-bearing.
 *
 * The type-only `ProxyHarness` edge stays where it is: it is erased at
 * compile time, so it adds no runtime cycle.
 */
import type { ApiSurface, ModelTiers } from '../../lib/tauri';

export const SURFACE_LABEL: Record<ApiSurface, string> = {
  anthropic: 'Anthropic',
  openai: 'OpenAI',
};

export const EMPTY_TIERS: ModelTiers = {
  default: null,
  small_fast: null,
  sonnet: null,
  opus: null,
  fable: null,
  haiku: null,
};

/** Claude / Anthropic-surface model tier fields (Harnesses attach/edit only). */
export const MODEL_TIER_FIELDS: { key: keyof ModelTiers; label: string }[] = [
  { key: 'default', label: 'Default model' },
  { key: 'fable', label: 'Fable' },
  { key: 'opus', label: 'Opus' },
  { key: 'sonnet', label: 'Sonnet' },
  { key: 'haiku', label: 'Haiku' },
  { key: 'small_fast', label: 'Small / fast' },
];