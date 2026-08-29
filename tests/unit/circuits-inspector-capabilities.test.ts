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
  });
});
