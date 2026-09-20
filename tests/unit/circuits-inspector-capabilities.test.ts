/**
 * Inspector helpers over the generated harness catalog (ADR-0037).
 *
 * Capability *values* are owned by Rust adapters and emitted into
 * `src/types/generated/HarnessCapabilitiesTable.ts`. This file must not
 * re-state those values as hand-typed literals — that was the placebo
 * drift gate this slice retires. Tests here cover lookup/effort logic
 * and the "no hand-copied capability table in src/" invariant.
 */

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
  effortAllowedFor,
  getCapabilitiesFor,
  HARNESS_CAPABILITIES,
  HARNESS_IDS,
  HARNESS_LABEL,
  HARNESS_PROFILE_ALIASES,
} from '../../src/components/Circuits/harnessCapabilities';
import type { HarnessCapabilities } from '../../src/types/generated/HarnessCapabilities';
import {
  HARNESS_CATALOG,
  type InspectorHarnessId,
} from '../../src/types/generated/HarnessCapabilitiesTable';

const srcRoot = join(dirname(fileURLToPath(import.meta.url)), '../../src');

function walkTsFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    const st = statSync(path);
    if (st.isDirectory()) return walkTsFiles(path);
    return name.endsWith('.ts') || name.endsWith('.tsx') ? [path] : [];
  });
}

function fixtureCaps(overrides: Partial<HarnessCapabilities> & Pick<HarnessCapabilities, 'effort_control'>): HarnessCapabilities {
  return {
    harness_id: 'fixture',
    supports_resume: false,
    auto_resume_on_startup: false,
    requires_attention_hook: false,
    attention_capability: { kind: 'none' },
    supports_passive_turn_watcher: false,
    produces_readable_transcript: false,
    supports_model_override: false,
    supports_effort_override: overrides.effort_control.kind !== 'none',
    supports_extra_args: false,
    supports_prefill: false,
    is_plain_terminal: false,
    available_on: [],
    ...overrides,
  };
}

describe('generated harness catalog (ADR-0037)', () => {
  it('exports every catalog id with a label and a capability row', () => {
    expect(HARNESS_IDS.length).toBeGreaterThan(0);
    expect(HARNESS_IDS).toEqual(HARNESS_CATALOG.ids);
    for (const id of HARNESS_IDS) {
      expect(HARNESS_LABEL[id], `missing label for ${id}`).toMatch(/^[A-Z]/);
      expect(HARNESS_CAPABILITIES[id], `missing capability entry for ${id}`).toBeDefined();
      expect(HARNESS_CAPABILITIES[id].harness_id).toBe(id);
    }
  });

  it('maps the claude profile id to the anthropic adapter', () => {
    expect(HARNESS_PROFILE_ALIASES.claude).toBe('anthropic');
    expect(getCapabilitiesFor('claude')).toBe(HARNESS_CAPABILITIES.anthropic);
  });
});

describe('getCapabilitiesFor', () => {
  it('returns null for empty, missing, and unknown ids', () => {
    expect(getCapabilitiesFor(null)).toBeNull();
    expect(getCapabilitiesFor(undefined)).toBeNull();
    expect(getCapabilitiesFor('')).toBeNull();
    expect(getCapabilitiesFor('not-a-harness')).toBeNull();
  });

  it('returns the generated row for a catalog id', () => {
    const id = HARNESS_IDS[0] as InspectorHarnessId;
    expect(getCapabilitiesFor(id)).toBe(HARNESS_CAPABILITIES[id]);
  });
});

describe('effortAllowedFor', () => {
  it('returns [] when effort control is none', () => {
    expect(effortAllowedFor(fixtureCaps({ effort_control: { kind: 'none' } }))).toEqual([]);
  });

  it('returns the closed vocabulary', () => {
    expect(
      effortAllowedFor(fixtureCaps({ effort_control: { kind: 'closed', allowed: ['low', 'high'] } })),
    ).toEqual(['low', 'high']);
  });

  it('returns the inline-config vocabulary', () => {
    expect(
      effortAllowedFor(
        fixtureCaps({
          effort_control: { kind: 'inline_config', key: 'model_reasoning_effort', allowed: ['none', 'xhigh'] },
        }),
      ),
    ).toEqual(['none', 'xhigh']);
  });
});

describe('no hand-typed capability table in src/', () => {
  it('does not assign supports_model_override literals outside generated artifacts', () => {
    const generated = `${relative(srcRoot, join(srcRoot, 'types/generated')).replaceAll('\\', '/')}/`;
    const violations: string[] = [];
    const assign = /supports_model_override\s*:/;
    for (const file of walkTsFiles(srcRoot)) {
      const rel = relative(srcRoot, file).replaceAll('\\', '/');
      if (rel.startsWith(generated) || rel.startsWith('types/generated/')) continue;
      const text = readFileSync(file, 'utf8');
      text.split('\n').forEach((line, i) => {
        if (assign.test(line)) {
          violations.push(`${rel}:${i + 1}: ${line.trim()}`);
        }
      });
    }
    expect(violations, `hand-typed capability literals remain:\n${violations.join('\n')}`).toEqual([]);
  });
});
