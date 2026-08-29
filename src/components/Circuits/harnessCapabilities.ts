/**
 * Hardcoded mirror of the Rust per-harness capability contract for the
 * Inspector's provider/model/effort/extra-args controls (issue #1358).
 *
 * The authoritative source of truth is `src-tauri/src/agent/capabilities.rs`
 * (the `inventory_matches_research_matrix` test pins it). The Inspector
 * reads from this static table because the Spawn Menu already gates by
 * `ProviderInfo.capabilities` for its own controls — we'd rather not add
 * a new Tauri command just to render a circuit author form.
 *
 * Drift gate: `tests/unit/circuits-inspector-capabilities.test.ts` asserts
 * every adapter id is present and the booleans / vocabulary match the
 * Rust inventory. Both suites run in `scripts/check.ps1 all`, so a
 * unilateral inventory change in either source trips the other.
 */

import type { EffortControlKind } from '../../types/generated/EffortControlKind';
import type { HarnessCapabilities } from '../../types/generated/HarnessCapabilities';

/** The harness ids the Inspector offers in its provider dropdown. */
export type InspectorHarnessId =
  | 'anthropic'
  | 'codex'
  | 'agy'
  | 'opencode'
  | 'grok';

const ANTHROPIC_CAPS: HarnessCapabilities = {
  harness_id: 'anthropic',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: true,
  produces_readable_transcript: true,
  supports_model_override: true,
  supports_effort_override: true,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: {
    kind: 'closed',
    allowed: ['low', 'medium', 'high'],
  },
  available_on: ['windows', 'macos', 'linux'],
};

const CODEX_CAPS: HarnessCapabilities = {
  harness_id: 'codex',
  supports_resume: true,
  auto_resume_on_startup: false,
  requires_attention_hook: true,
  produces_readable_transcript: true,
  supports_model_override: true,
  supports_effort_override: true,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: {
    kind: 'inline_config',
    key: 'model_reasoning_effort',
    allowed: ['none', 'low', 'medium', 'high', 'xhigh'],
  },
  available_on: ['windows', 'macos', 'linux'],
};

const AGY_CAPS: HarnessCapabilities = {
  harness_id: 'agy',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: true,
  produces_readable_transcript: true,
  supports_model_override: true,
  supports_effort_override: true,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: {
    kind: 'closed',
    allowed: ['low', 'medium', 'high'],
  },
  available_on: ['windows', 'macos', 'linux'],
};

const OPENCODE_CAPS: HarnessCapabilities = {
  harness_id: 'opencode',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  produces_readable_transcript: false,
  supports_model_override: true,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  available_on: ['windows', 'macos', 'linux'],
};

const GROK_CAPS: HarnessCapabilities = {
  harness_id: 'grok',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: true,
  produces_readable_transcript: true,
  supports_model_override: true,
  supports_effort_override: true,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: {
    kind: 'closed',
    allowed: ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'],
  },
  available_on: ['windows', 'macos', 'linux'],
};

/**
 * The harness-to-capability map. Mirrors the Rust inventory table
 * exactly — see `tests/unit/circuits-inspector-capabilities.test.ts`
 * for the drift gate.
 */
export const HARNESS_CAPABILITIES: Record<InspectorHarnessId, HarnessCapabilities> = {
  anthropic: ANTHROPIC_CAPS,
  codex: CODEX_CAPS,
  agy: AGY_CAPS,
  opencode: OPENCODE_CAPS,
  grok: GROK_CAPS,
};

/**
 * Human-readable harness label for the Inspector's provider dropdown.
 * Mirrors the `UiMeta::label` declared by each Rust adapter but kept
 * as a static table (a Tauri round-trip just for a label would be
 * wasteful — the same shape is also used by the Spawn Menu, which gets
 * the label via `ProviderInfo`).
 */
export const HARNESS_LABEL: Record<InspectorHarnessId, string> = {
  anthropic: 'Claude Code',
  codex: 'Codex',
  agy: 'Antigravity',
  opencode: 'OpenCode',
  grok: 'Grok Code',
};

/**
 * The Inspector's `default` option corresponds to a `null` provider on
 * the SpawnAgentNode — fall through to the mesh's default. Returns
 * `null` so the rest of the form knows to render the "no overrides"
 * fallback (no model / effort / extra-args inputs).
 */
export function getCapabilitiesFor(
  harnessId: string | null | undefined,
): HarnessCapabilities | null {
  if (!harnessId) return null;
  return HARNESS_CAPABILITIES[harnessId as InspectorHarnessId] ?? null;
}

/**
 * The full vocabulary for a given harness's effort select, used by the
 * Inspector's `<select>` children. Returns `[]` for harnesses with no
 * effort control so the dropdown renders the empty state.
 */
export function effortAllowedFor(
  caps: HarnessCapabilities,
): string[] {
  const ctl = caps.effort_control;
  if (ctl.kind === 'none') return [];
  return ctl.allowed;
}

/**
 * Distinguish `Closed` vs `InlineConfig` vs `None` for the Inspector's
 * effort label copy. Re-exported for the test file.
 */
export type InspectorEffortKind = EffortControlKind['kind'];
