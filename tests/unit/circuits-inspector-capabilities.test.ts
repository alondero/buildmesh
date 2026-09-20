/**
 * Drift gate for `src/components/Circuits/harnessCapabilities.ts`.
 *
 * Each entry below is the *full* expected `HarnessCapabilities` literal
 * for one harness id. A single `toEqual` per harness asserts every
 * field of the `HarnessCapabilities` struct — an unasserted field is
 * therefore impossible: any drift between this expected table and the
 * exported `HARNESS_CAPABILITIES` trips the gate.
 *
 * **What this test catches.** TS-only changes to
 * `harnessCapabilities.ts` — both intentional fixes and accidental
 * regressions. Touching a value in the mirror requires touching this
 * table in the same PR; the `toEqual` failure message names both
 * files.
 *
 * **What this test does NOT catch.** A Rust-only change that updates
 * both the adapter AND its `inventory_matches_research_matrix` pin
 * test. In that case both `cargo test` and `vitest` pass while the TS
 * mirror is silently stale. Closing that loop requires generating
 * this expected literal from Rust (e.g. via a `cargo test` step that
 * serializes the per-adapter `HarnessCapabilities` to
 * `src/types/generated/harnessCapabilities.json`, with this test
 * asserting equality against that generated artifact). That is the
 * follow-on slice described in the PR.
 *
 * The expected literal in this file was sourced from
 * `src/components/Circuits/harnessCapabilities.ts` after the opencode,
 * dsh, and muse corrections from issue #1358 / #1296 / #1708. The
 * canonical matrix is documented in
 * `docs/learning/harness-capabilities-matrix.md`.
 */

import { describe, expect, it } from 'vitest';
import {
  HARNESS_CAPABILITIES,
  HARNESS_LABEL,
  type HarnessCapabilities,
  type InspectorHarnessId,
} from '../../src/components/Circuits/harnessCapabilities';

/**
 * The full expected `HarnessCapabilities` literal per harness id.
 * One entry per `InspectorHarnessId`; the entries here are the
 * canonical truth the data-driven `toEqual` compares against.
 *
 * The harness order matches the array order in
 * `BUILTIN_HARNESS_IDS` (see `src-tauri/src/agent/provider/mod.rs`)
 * for human-readability; the test does not depend on order.
 */
const EXPECTED_HARNESS_CAPABILITIES: Record<InspectorHarnessId, HarnessCapabilities> = {
  // Anthropic (Claude Code) — skip-permissions hook, closed effort
  // vocabulary, available on all three platforms.
  anthropic: {
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
  },

  // Codex — `inline_config` effort via `model_reasoning_effort`.
  codex: {
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
    available_on: ['macos', 'windows', 'linux'],
  },

  // AGY (Antigravity) — skip-permissions hook, stop-only signal,
  // closed `low|medium|high` effort vocabulary.
  agy: {
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
    available_on: ['windows', 'linux', 'macos'],
  },

  // OpenCode — plugin hook under issue #1295 (turn-completed +
  // question/permission prompts); transcript reader wired in #1296.
  opencode: {
    harness_id: 'opencode',
    supports_resume: true,
    auto_resume_on_startup: true,
    requires_attention_hook: true,
    attention_capability: {
      kind: 'hook',
      events: ['turn_completed', 'question_requested', 'permission_requested'],
      launch_mode: 'permission_ask',
      trust: null,
      min_version: null,
    },
    supports_passive_turn_watcher: false,
    produces_readable_transcript: true,
    supports_model_override: true,
    supports_effort_override: false,
    supports_extra_args: true,
    supports_prefill: true,
    is_plain_terminal: false,
    effort_control: { kind: 'none' },
    available_on: ['windows', 'linux', 'macos'],
  },

  // Grok — global hook dir, closed effort vocabulary
  // `none|minimal|low|medium|high|xhigh|max`.
  grok: {
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
    available_on: ['windows', 'linux', 'macos'],
  },

  // Cursor — model yes, effort no, prefill yes (issue #1143).
  // Cursor ships an attention hook under `--force` (issue #1368
  // round-2) mirroring AGY's skip-permissions shape. `trust` is `null`
  // because Cursor under `--force` does not require an explicit
  // workspace-trust entry.
  cursor: {
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
    available_on: ['windows', 'linux', 'macos'],
  },

  // Kimi — interactive TUI, model yes, no effort, no prefill.
  kimi: {
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
    available_on: ['windows', 'linux', 'macos'],
  },

  // mcode — interactive TUI; model OFF (issue #1179), effort OFF,
  // prefill yes; readable messages.jsonl transcript wired
  // (TranscriptFormat::Mcode).
  mcode: {
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
    available_on: ['windows', 'linux', 'macos'],
  },

  // dsh (DeepSeek Harness) — gates resume, model, and prefill to
  // false; extras accepted because the CLI forwards positional args.
  dsh: {
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
    available_on: ['windows', 'linux', 'macos'],
  },

  // Command Code — passive turn watcher (issue #1481), closed
  // `low|medium|high` effort vocabulary.
  commandcode: {
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
  },

  // Freebuff — interactive AI coding agent; model OFF, effort OFF,
  // prefill yes.
  freebuff: {
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
  },

  // Terminal — the plain-shell harness; every override OFF (issue
  // #1358 declared `supports_extra_args: false` so the resolver
  // drops it; splicing synthetic flags into a user's interactive
  // shell session is a footgun).
  terminal: {
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
  },

  // Muse — Windows landed in Muse 1.3.0. Durable per-session JSONL
  // reader wired in `services::transcript_reader::adapters::muse`
  // (issue #1708). Passive session-log watcher supplies the turn
  // signal (issue #1709) since Muse exposes no native attention
  // hook. Order mirrors `MuseAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/muse.rs`:
  // `[Platform::Linux, Platform::Macos, Platform::Windows]`.
  muse: {
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
    available_on: ['linux', 'macos', 'windows'],
  },

  // Cline (issue #1773) — Native Provider: resume + model + effort
  // + prefill. Attention (#1775) and the transcript reader (#1776)
  // are not shipped in this slice, so those flags stay honest-empty.
  // Effort is the closed `--thinking` vocabulary verified against
  // Cline 3.0.62. Order mirrors `ClineAdapter::available_on()` in
  // `src-tauri/src/agent/provider/adapters/cline.rs`.
  cline: {
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
    available_on: ['windows', 'linux', 'macos'],
  },
};

const REQUIRED_HARNESSES = Object.keys(EXPECTED_HARNESS_CAPABILITIES) as InspectorHarnessId[];

describe('harnessCapabilities.ts drift gate (issue #1358)', () => {
  it('exposes every expected harness id (no missing entries)', () => {
    const exposed = new Set(Object.keys(HARNESS_CAPABILITIES));
    expect(exposed.size).toBe(REQUIRED_HARNESSES.length);
    for (const id of REQUIRED_HARNESSES) {
      expect(
        exposed.has(id),
        `${id} is in the expected inventory but missing from HARNESS_CAPABILITIES`,
      ).toBe(true);
    }
  });

  it('exposes only expected harness ids (no extra entries)', () => {
    const exposed = new Set(Object.keys(HARNESS_CAPABILITIES));
    const expected = new Set(REQUIRED_HARNESSES);
    for (const id of exposed) {
      expect(
        expected.has(id as InspectorHarnessId),
        `${id} is exported from HARNESS_CAPABILITIES but is not in the expected inventory`,
      ).toBe(true);
    }
  });

  it('every entry has a human label for the dropdown', () => {
    for (const id of REQUIRED_HARNESSES) {
      expect(HARNESS_LABEL[id], `missing label for ${id}`).toMatch(/^[A-Z]/);
    }
  });

  // Data-driven comparison — one `toEqual` per harness, against the
  // full expected literal. Every field of every entry is asserted,
  // so an unasserted field is impossible. Touching a value in
  // `harnessCapabilities.ts` requires touching this table.
  for (const id of REQUIRED_HARNESSES) {
    it(`${id} capabilities exactly match the expected HarnessCapabilities literal`, () => {
      expect(HARNESS_CAPABILITIES[id]).toEqual(EXPECTED_HARNESS_CAPABILITIES[id]);
    });
  }
});
