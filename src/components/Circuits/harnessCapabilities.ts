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
 * **Drift gate (be honest).** `tests/unit/circuits-inspector-capabilities.test.ts`
 * asserts that this file matches a static, hand-typed expected literal
 * — it is an **internal integrity check on the TS mirror only**. It
 * does not read Rust. So the gate catches:
 *   - TS-only changes (this file changes, the test fails).
 *
 * It does NOT catch:
 *   - Rust-only changes where the adapter AND its pin test are updated
 *     together. Both the Rust `inventory_matches_research_matrix` test
 *     and the TS test pass, but the TS mirror is now stale and the
 *     Inspector renders the wrong UI.
 *
 * That second failure mode is the open loop. Closing it requires
 * generating this mirror from Rust (e.g. via a `cargo test` step that
 * serializes the per-adapter `HarnessCapabilities` to
 * `src/types/generated/harnessCapabilities.json`, with the TS test
 * asserting equality against that generated artifact). That is the
 * follow-on slice described in the PR — this slice makes the existing
 * gate honest without entrenching the mirror further.
 *
 * The current matrix of declared fields is documented in
 * `docs/learning/harness-capabilities-matrix.md`.
 *
 * Issue #1362 review note: when a future slice grows
 * `Provider::adapter()`'s surface (e.g. one of these 14 harnesses
 * changes its capability boolean), both this file AND the Rust
 * inventory must update in lockstep.
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
  | 'muse'
  | 'cline'
  | 'terminal';

const ANTHROPIC_CAPS: HarnessCapabilities = {
  harness_id: 'anthropic',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: true,
  attention_capability: {
    kind: 'hook',
    events: ['turn_completed', 'input_required', 'background_running'],
    launch_mode: 'skip_permissions',
    trust: 'workspace trust',
    min_version: null,
  },
  supports_passive_turn_watcher: false,
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
  auto_resume_on_startup: true,
  requires_attention_hook: true,
  attention_capability: {
    kind: 'hook',
    events: [
      'turn_completed',
      'input_required',
      'permission_requested',
      'background_running',
    ],
    launch_mode: 'permission_ask',
    trust: 'codex project trust (#1379)',
    min_version: null,
  },
  supports_passive_turn_watcher: false,
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
  // Order matches `CodexAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/codex.rs`:
  // `[Platform::Macos, Platform::Windows, Platform::Linux]`.
  available_on: ['macos', 'windows', 'linux'],
};

// AGY (Antigravity) — skip-permissions hook, stop-only signal,
// closed `low|medium|high` effort vocabulary.
const AGY_CAPS: HarnessCapabilities = {
  harness_id: 'agy',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: true,
  attention_capability: {
    kind: 'hook',
    events: ['turn_completed', 'background_running'],
    launch_mode: 'skip_permissions',
    trust: 'workspace trust',
    min_version: '1.0.0',
  },
  supports_passive_turn_watcher: false,
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
  // Order matches `AgyAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/agy.rs`:
  // `[Platform::Windows, Platform::Linux, Platform::Macos]`.
  available_on: ['windows', 'linux', 'macos'],
};

const OPENCODE_CAPS: HarnessCapabilities = {
  harness_id: 'opencode',
  supports_resume: true,
  auto_resume_on_startup: true,
  // Issue #1295 — plugin hook unblocks the Autopilot gate; the hook
  // emits a turn-completed signal and the interactive permission-ask
  // for question/permission prompts.
  requires_attention_hook: true,
  attention_capability: {
    kind: 'hook',
    events: ['turn_completed', 'question_requested', 'permission_requested'],
    launch_mode: 'permission_ask',
    trust: null,
    min_version: null,
  },
  supports_passive_turn_watcher: false,
  // Issue #1296 — OpenCode now produces a readable transcript via the
  // SQLite reader; Coordinator Node Digest hydrates and the archived-
  // node resume picker surfaces OpenCode rows.
  produces_readable_transcript: true,
  supports_model_override: true,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  // Order matches `OpencodeAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/opencode.rs`:
  // `[Platform::Windows, Platform::Linux, Platform::Macos]`.
  available_on: ['windows', 'linux', 'macos'],
};

const GROK_CAPS: HarnessCapabilities = {
  harness_id: 'grok',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: true,
  attention_capability: {
    kind: 'hook',
    events: [
      'turn_completed',
      'input_required',
      'permission_requested',
      'question_requested',
    ],
    launch_mode: 'permission_ask',
    trust: 'global hook dir',
    min_version: '1.0.5',
  },
  supports_passive_turn_watcher: false,
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
  // Order matches `GrokAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/grok.rs`:
  // `[Platform::Windows, Platform::Linux, Platform::Macos]`.
  available_on: ['windows', 'linux', 'macos'],
};

// Cursor — model yes, effort no, prefill yes (issue #1143). Issue
// #1368 round-2: Cursor now ships an attention hook under `--force`.
// The descriptor mirrors AGY's skip-permissions shape (stop-only
// signal, `min_version` pinned to `CURSOR_MIN_HOOK_VERSION`). The
// `trust` field is `null` rather than `'workspace trust'` because
// Cursor under `--force` does not require an explicit workspace-trust
// entry; advertising a non-empty value would promise a step the
// adapter never takes (round-2 review point 2).
const CURSOR_CAPS: HarnessCapabilities = {
  harness_id: 'cursor',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: true,
  attention_capability: {
    kind: 'hook',
    events: ['turn_completed', 'background_running'],
    launch_mode: 'skip_permissions',
    trust: null,
    min_version: '1.0.0',
  },
  supports_passive_turn_watcher: false,
  produces_readable_transcript: true,
  supports_model_override: true,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  // Order matches `CursorAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/cursor.rs`:
  // `[Platform::Windows, Platform::Linux, Platform::Macos]`.
  available_on: ['windows', 'linux', 'macos'],
};

// Kimi — interactive TUI like Grok/Anthropic, model yes, no effort, no prefill
const KIMI_CAPS: HarnessCapabilities = {
  harness_id: 'kimi',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  attention_capability: { kind: 'none' },
  supports_passive_turn_watcher: false,
  produces_readable_transcript: false,
  supports_model_override: true,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: false,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  // Order matches `KimiAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/kimi.rs`:
  // `[Platform::Windows, Platform::Linux, Platform::Macos]`.
  available_on: ['windows', 'linux', 'macos'],
};

// mcode — interactive TUI; model OFF (issue #1179), effort OFF, prefill yes; readable transcript wired
const MCODE_CAPS: HarnessCapabilities = {
  harness_id: 'mcode',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  attention_capability: { kind: 'none' },
  supports_passive_turn_watcher: false,
  produces_readable_transcript: true,
  supports_model_override: false,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  // Order matches `McodeAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/mcode.rs`:
  // `[Platform::Windows, Platform::Linux, Platform::Macos]`.
  available_on: ['windows', 'linux', 'macos'],
};

// dsh (DeepSeek Harness) — no resume, no model, no effort, no prefill
const DSH_CAPS: HarnessCapabilities = {
  harness_id: 'dsh',
  supports_resume: false,
  auto_resume_on_startup: false,
  requires_attention_hook: false,
  attention_capability: { kind: 'none' },
  supports_passive_turn_watcher: false,
  produces_readable_transcript: false,
  supports_model_override: false,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: false,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  // Order matches `DshAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/dsh.rs`:
  // `[Platform::Windows, Platform::Linux, Platform::Macos]`.
  available_on: ['windows', 'linux', 'macos'],
};

const COMMANDCODE_CAPS: HarnessCapabilities = {
  harness_id: 'commandcode',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  attention_capability: { kind: 'none' },
  supports_passive_turn_watcher: true,
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
  attention_capability: { kind: 'none' },
  supports_passive_turn_watcher: false,
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
  attention_capability: { kind: 'none' },
  supports_passive_turn_watcher: false,
  produces_readable_transcript: false,
  supports_model_override: false,
  supports_effort_override: false,
  supports_extra_args: false,
  supports_prefill: false,
  is_plain_terminal: true,
  effort_control: { kind: 'none' },
  available_on: ['windows', 'macos', 'linux'],
};

// Muse — Windows landed in Muse 1.3.0 (issue #1708 added the
// transcript reader; the Windows build of Muse is now usable).
// Durable per-session JSONL reader wired in
// `services::transcript_reader::adapters::muse`. Issue #1709:
// no native attention hook, but the backend session-log watcher supplies
// the turn signal — mirrors `adapters::MUSE` (passive turn watcher true).
const MUSE_CAPS: HarnessCapabilities = {
  harness_id: 'muse',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  attention_capability: { kind: 'none' },
  supports_passive_turn_watcher: true,
  produces_readable_transcript: true,
  supports_model_override: true,
  supports_effort_override: false,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: { kind: 'none' },
  // Order matches `MuseAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/muse.rs`:
  // `[Platform::Linux, Platform::Macos, Platform::Windows]`.
  available_on: ['linux', 'macos', 'windows'],
};

// Cline (issue #1773) — Native Provider: resume + model + effort + prefill,
// but no attention hook (#1775) and no readable transcript (#1776) in this
// slice, so both stay honest-empty. Effort is the closed `--thinking`
// vocabulary (verified against Cline 3.0.62 `--help`).
const CLINE_CAPS: HarnessCapabilities = {
  harness_id: 'cline',
  supports_resume: true,
  auto_resume_on_startup: true,
  requires_attention_hook: false,
  attention_capability: { kind: 'none' },
  supports_passive_turn_watcher: false,
  produces_readable_transcript: false,
  supports_model_override: true,
  supports_effort_override: true,
  supports_extra_args: true,
  supports_prefill: true,
  is_plain_terminal: false,
  effort_control: {
    kind: 'closed',
    allowed: ['none', 'low', 'medium', 'high', 'xhigh'],
  },
  // Order matches `ClineAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/cline.rs`.
  available_on: ['windows', 'linux', 'macos'],
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
  muse: MUSE_CAPS,
  cline: CLINE_CAPS,
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
  muse: 'Meta Muse',
  cline: 'Cline',
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
