/**
 * Drift gate for `src/components/Circuits/harnessCapabilities.ts`
 * (issue #1358 / slice 3 of #1355).
 *
 * The Rust `inventory_matches_research_matrix` test in
 * `src-tauri/src/agent/capabilities.rs` pins the per-adapter boolean
 * flags and `EffortControlKind` vocabulary. This vitest mirrors the
 * same per-adapter inventory on the TS side so the Inspector's
 * capability-gated controls stay in sync with the Rust resolver's
 * capability mask. CI's `scripts/check.ps1 all` runs both suites in
 * the same pipeline so a unilateral change in either source trips
 * the other.
 *
 * Touching either side requires touching the other. The failure
 * message when these two diverge names both files.
 */

import { describe, expect, it } from 'vitest';
import {
  effortAllowedFor,
  HARNESS_CAPABILITIES,
  HARNESS_LABEL,
  type InspectorHarnessId,
} from '../../src/components/Circuits/harnessCapabilities';

const REQUIRED_HARNESSES: InspectorHarnessId[] = [
  'anthropic',
  'codex',
  'agy',
  'opencode',
  'grok',
  'cursor',
  'kimi',
  'mcode',
  'dsh',
  'commandcode',
  'freebuff',
  'terminal',
];

describe('harnessCapabilities.ts ↔ Rust inventory drift gate (issue #1358)', () => {
  it('exports every harness id the Inspector offers', () => {
    for (const id of REQUIRED_HARNESSES) {
      expect(HARNESS_CAPABILITIES[id], `missing capability entry for ${id}`).toBeDefined();
    }
  });

  it('every entry has a human label for the dropdown', () => {
    for (const id of REQUIRED_HARNESSES) {
      expect(HARNESS_LABEL[id], `missing label for ${id}`).toMatch(/^[A-Z]/);
    }
  });

  // Per-adapter capability flags — mirrors the Rust inventory table.
  it('Anthropic (Claude Code) matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.anthropic;
    expect(c.harness_id).toBe('anthropic');
    expect(c.supports_model_override).toBe(true);
    expect(c.supports_effort_override).toBe(true);
    expect(c.supports_extra_args).toBe(true);
    expect(c.is_plain_terminal).toBe(false);
    expect(c.effort_control.kind).toBe('closed');
    expect(effortAllowedFor(c)).toEqual(['low', 'medium', 'high']);
  });

  it('Codex matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.codex;
    expect(c.harness_id).toBe('codex');
    expect(c.supports_model_override).toBe(true);
    expect(c.supports_effort_override).toBe(true);
    expect(c.supports_extra_args).toBe(true);
    expect(c.effort_control.kind).toBe('inline_config');
    if (c.effort_control.kind === 'inline_config') {
      expect(c.effort_control.key).toBe('model_reasoning_effort');
    }
    expect(effortAllowedFor(c)).toEqual(
      expect.arrayContaining(['none', 'low', 'medium', 'high', 'xhigh']),
    );
  });

  it('AGY (Antigravity) matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.agy;
    expect(c.harness_id).toBe('agy');
    expect(c.supports_model_override).toBe(true);
    expect(c.supports_effort_override).toBe(true);
    expect(c.supports_extra_args).toBe(true);
    expect(c.effort_control.kind).toBe('closed');
    expect(effortAllowedFor(c)).toEqual(['low', 'medium', 'high']);
  });

  it('OpenCode matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.opencode;
    expect(c.harness_id).toBe('opencode');
    expect(c.supports_model_override).toBe(true);
    expect(c.supports_effort_override).toBe(false);
    expect(c.supports_extra_args).toBe(true);
    expect(c.effort_control.kind).toBe('none');
    expect(effortAllowedFor(c)).toEqual([]);
  });

  it('Grok matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.grok;
    expect(c.harness_id).toBe('grok');
    expect(c.supports_model_override).toBe(true);
    expect(c.supports_effort_override).toBe(true);
    expect(c.supports_extra_args).toBe(true);
    expect(c.effort_control.kind).toBe('closed');
    expect(effortAllowedFor(c)).toEqual(
      expect.arrayContaining(['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max']),
    );
      // Issue #1366: pin Grok's min_version via vitest so a
    // flip-back to null trips the gate here, not just the
    // Rust inventory. (round-3 N1 follow-up.)
    expect(c.attention_capability).toEqual({
      kind: 'hook',
      events: expect.arrayContaining([
        'turn_completed',
        'input_required',
        'permission_requested',
        'question_requested',
      ]),
      launch_mode: 'permission_ask',
      trust: 'global hook dir',
      min_version: '1.0.5',
    });
});

  // Cursor — model yes, effort no, prefill yes (issue #1143).
  // Issue #1368 round-2: Cursor now ships an attention hook under
  // `--force`, mirroring AGY's skip-permissions shape. The vitest pin
  // catches a flip-back to `requires_attention_hook: false` (round-2
  // review N1 pattern from Grok issue #1366) AND the `trust` field
  // must be `null` — Cursor under `--force` does not require an
  // explicit workspace-trust entry (review point 2).
  it('Cursor matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.cursor;
    expect(c.harness_id).toBe('cursor');
    expect(c.supports_model_override).toBe(true);
    expect(c.supports_effort_override).toBe(false);
    expect(c.supports_extra_args).toBe(true);
    expect(c.supports_prefill).toBe(true);
    expect(c.requires_attention_hook).toBe(true);
    expect(c.attention_capability).toEqual({
      kind: 'hook',
      events: expect.arrayContaining([
        'turn_completed',
        'background_running',
      ]),
      launch_mode: 'skip_permissions',
      trust: null,
      min_version: '1.0.0',
    });
    expect(c.effort_control.kind).toBe('none');
    expect(effortAllowedFor(c)).toEqual([]);
  });

  // Kimi — model yes, no effort, no prefill (issue #918 / #911).
  it('Kimi matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.kimi;
    expect(c.harness_id).toBe('kimi');
    expect(c.supports_model_override).toBe(true);
    expect(c.supports_effort_override).toBe(false);
    expect(c.supports_extra_args).toBe(true);
    expect(c.supports_prefill).toBe(false);
    expect(c.effort_control.kind).toBe('none');
  });

  // mcode — interaction TUI; model OFF (issue #1179), effort OFF, prefill yes.
  it('mcode matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.mcode;
    expect(c.harness_id).toBe('mcode');
    expect(c.supports_model_override).toBe(false);
    expect(c.supports_effort_override).toBe(false);
    expect(c.supports_extra_args).toBe(true);
    expect(c.supports_prefill).toBe(true);
    expect(c.effort_control.kind).toBe('none');
  });

  // dsh — model yes, no effort, no prefill.
  it('dsh matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.dsh;
    expect(c.harness_id).toBe('dsh');
    expect(c.supports_model_override).toBe(true);
    expect(c.supports_effort_override).toBe(false);
    expect(c.supports_extra_args).toBe(true);
    expect(c.supports_prefill).toBe(false);
    expect(c.effort_control.kind).toBe('none');
  });

  it('Command Code matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.commandcode;
    expect(c.harness_id).toBe('commandcode');
    expect(c.supports_model_override).toBe(true);
    expect(c.supports_effort_override).toBe(true);
    expect(c.supports_extra_args).toBe(true);
    expect(c.supports_prefill).toBe(true);
    expect(c.produces_readable_transcript).toBe(true);
    expect(c.effort_control.kind).toBe('closed');
    expect(effortAllowedFor(c)).toEqual(['low', 'medium', 'high']);
  });

  it('Freebuff matches the Rust inventory', () => {
    const c = HARNESS_CAPABILITIES.freebuff;
    expect(c.harness_id).toBe('freebuff');
    expect(c.supports_model_override).toBe(false);
    expect(c.supports_effort_override).toBe(false);
    expect(c.supports_extra_args).toBe(true);
    expect(c.supports_prefill).toBe(true);
    expect(c.effort_control.kind).toBe('none');
  });

  // Terminal — plain shell; every override OFF. The issue #1362 review
  // caveat: splicing synthetic flags into a user's interactive shell
  // session is a footgun, hence `supports_extra_args: false`.
  it('Terminal matches the Rust inventory (plain shell, no overrides)', () => {
    const c = HARNESS_CAPABILITIES.terminal;
    expect(c.harness_id).toBe('terminal');
    expect(c.is_plain_terminal).toBe(true);
    expect(c.supports_model_override).toBe(false);
    expect(c.supports_effort_override).toBe(false);
    expect(c.supports_extra_args).toBe(false);
    expect(c.supports_prefill).toBe(false);
    expect(c.effort_control.kind).toBe('none');
  });

  // Drift gate invariant: the set of inspector-visible harness ids
  // matches BUILTIN_HARNESS_IDS exactly (modulo legacy aliases). Any
  // future harness added to Rust must be added here in the same PR.
  // The vitest is the FAIL-CLOSED enforcement; the test file lists the
  // same set explicitly above and here.
  it('Inspector exposes every BUILTIN_HARNESS_IDS adapter', () => {
    const exposed = new Set(Object.keys(HARNESS_CAPABILITIES));
    expect(exposed.size).toBe(REQUIRED_HARNESSES.length);
    for (const id of REQUIRED_HARNESSES) {
      expect(exposed.has(id), `${id} is a Rust BUILTIN_HARNESS_IDS adapter and must have a TS entry`).toBe(true);
    }
  });
});
