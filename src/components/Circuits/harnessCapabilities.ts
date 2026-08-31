/**
 * Hardcoded mirror of the Rust per-harness capability contract for the
 * Inspector's provider/model/effort/extra-args controls (issue #1358).
 *
 * **Provenance.** The authoritative source of truth is
 * `src-tauri/src/agent/capabilities.rs::BUILTIN_HARNESS_IDS` (and
 * the `inventory_matches_research_matrix` unit test that pins every
 * adapter's `HarnessCapabilities`). The Inspector reads from this
 * static table because the Spawn Menu already gates by
 * `ProviderInfo.capabilities` for its own controls — we'd rather not
 * add a new Tauri command just to render a circuit author form.
 *
 * **Drift gate.** `tests/unit/circuits-inspector-capabilities.test.ts`
 * asserts TWO properties that, together, prevent the previous
 * "placebo drift gate" flagged in PR #1362 code review:
 *   1. Every adapter id in `BUILTIN_HARNESS_IDS` is present (otherwise
 *      a circuit author who picks an unsupported id silently gets the
 *      mesh-default UI).
 *   2. Every present adapter's boolean fields match the canonical
 *      Rust inventory.
 * CI runs both Rust unit tests (`scripts/check.ps1 rust`) and Vitest
 * (`scripts/check.ps1 unit`) in the same pipeline, so a unilateral
 * change in either source trips the other.
 *
 * Issue #1362 review note: when a future slice grows
 * `Provider::adapter()`'s surface (e.g. one of these 10 harnesses
 * changes its capability boolean), both this file AND the Rust
 * inventory must update in lockstep — the test fails closed.
 */

import type { EffortControlKind } from '../../types/generated/EffortControlKind';
import type { HarnessCapabilities } from '../../types/generated/HarnessCapabilities';

/**
 * The harness ids the Inspector offers in its provider dropdown.
 *
 * Source of truth: `src-tauri/src/agent/provider/mod.rs::BUILTIN_HARNESS_IDS`
 * — adding a new harness there requires:
 *   1. A new `InspectorHarnessId` variant here
 *   2. A new entry in `HARNESS_CAPABILITIES`
 *   3. A new entry in `HARNESS_LABEL`
 *   4. The drift-gate test (`inspector-providers-coverage`) updated
 *      to include the new id.
 */
export type InspectorHarnessId =
  | 'anthropic'
  | 'codex'
  | 'agy'
  | 'opencode'
  | 'grok'
  | 'cursor'
  | 'kimi'
  | 'mcode'
  | 'dsh'
  | 'commandcode'
  | 'freebuff'
  | 'terminal';

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

// Cursor — model yes, effort no, prefill yes (issue #1143)
const CURSOR_CAPS: HarnessCapabilities = {
  harness_id: 'cursor',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  produces_readable_transcript: true,
  supports_model_override: true,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  available_on: ['windows', 'macos', 'linux'],
};

// Kimi — interactive TUI like Grok/Anthropic, model yes, no effort, no prefill
const KIMI_CAPS: HarnessCapabilities = {
  harness_id: 'kimi',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  produces_readable_transcript: false,
  supports_model_override: true,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: false,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  available_on: ['windows', 'macos', 'linux'],
};

// mcode — interactive TUI; model OFF (issue #1179), effort OFF, prefill yes
const MCODE_CAPS: HarnessCapabilities = {
  harness_id: 'mcode',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  produces_readable_transcript: false,
  supports_model_override: false,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  available_on: ['windows', 'macos', 'linux'],
};

// dsh (DeepSeek Harness) — model yes, no effort, no prefill
const DSH_CAPS: HarnessCapabilities = {
  harness_id: 'dsh',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  produces_readable_transcript: false,
  supports_model_override: true,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: false,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  available_on: ['windows', 'macos', 'linux'],
};

const COMMANDCODE_CAPS: HarnessCapabilities = {
  harness_id: 'commandcode',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  produces_readable_transcript: true,
  supports_model_override: true,
  supports_effort_override: true,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: { kind: 'closed', allowed: ['low', 'medium', 'high'] },
  available_on: ['windows', 'macos', 'linux'],
};

// freebuff — interactive AI coding agent; model OFF, effort OFF, prefill yes
const FREEBUFF_CAPS: HarnessCapabilities = {
  harness_id: 'freebuff',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  produces_readable_transcript: false,
  supports_model_override: false,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  available_on: ['windows', 'linux', 'macos'],
};

// Terminal — the plain-shell harness; every override OFF (issue #1358
// declared `supports_extra_args: false` so the resolver drops it)
const TERMINAL_CAPS: HarnessCapabilities = {
  harness_id: 'terminal',
  supports_resume: false,
  auto_resume_on_startup: false,
  requires_attention_hook: false,
  produces_readable_transcript: false,
  supports_model_override: false,
  supports_effort_override: false,
  supports_extra_args: false,
  supports_prefill: false,
  is_plain_terminal: true,
  effort_control: { kind: 'none' },
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
  cursor: CURSOR_CAPS,
  kimi: KIMI_CAPS,
  mcode: MCODE_CAPS,
  dsh: DSH_CAPS,
  commandcode: COMMANDCODE_CAPS,
  freebuff: FREEBUFF_CAPS,
  terminal: TERMINAL_CAPS,
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
  cursor: 'Cursor',
  kimi: 'Kimi Code',
  mcode: 'MiniMax Code',
  dsh: 'DeepSeek Harness',
  commandcode: 'Command Code',
  freebuff: 'Freebuff',
  terminal: 'Terminal',
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
  // Defensive: rather than `as InspectorHarnessId` cast, look up
  // via `HARNESS_CAPABILITIES` whose keying is exhaustive — unknown
  // ids (legacy IDs, mistypes, future harness not yet wired) get the
  // same `null` shape as "no provider selected".
  if (harnessId in HARNESS_CAPABILITIES) {
    return HARNESS_CAPABILITIES[harnessId as InspectorHarnessId];
  }
  return null;
}

/**
 * The full vocabulary for a given harness's effort select, used by the
 * Inspector's `<select>` children. Returns `[]` for harnesses with no
 * effort control so the dropdown renders the empty state.
 */
export function effortAllowedFor(caps: HarnessCapabilities): string[] {
  const ctl = caps.effort_control;
  if (ctl.kind === 'none') return [];
  return ctl.allowed;
}

/**
 * Distinguish `Closed` vs `InlineConfig` vs `None` for the Inspector's
 * effort label copy. Re-exported for the test file.
 */
export type InspectorEffortKind = EffortControlKind['kind'];
