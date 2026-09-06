import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

import {
  checkReadmeDrift,
  parseProviderVariants,
  parseHarnessLabel,
  parseHarnessLabel as _parseHarnessLabel,
  numberToWord,
  featuresMultiAgentSection,
} from '../../scripts/check-readme-drift.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));

// Real source-of-truth files used as inputs. The drift gate reads
// these, so the tests run against the same artifacts the gate does —
// when a real PR adds a 13th provider, both gate and tests see it
// without anyone touching the fixtures.
const realProviderTypesPath = join(root, 'src/types/generated/Provider.ts');
const realHarnessLabelPath = join(root, 'src/components/Circuits/harnessCapabilities.ts');
const realReadmePath = join(root, 'README.md');

const realProviderTypes = readFileSync(realProviderTypesPath, 'utf8');
const realHarnessLabelTs = readFileSync(realHarnessLabelPath, 'utf8');
const realHarnessLabelMap = parseHarnessLabel(realHarnessLabelTs);
const realReadme = readFileSync(realReadmePath, 'utf8');

// Build a synthetic Provider + HARNESS_LABEL fixture whose variant
// count is whatever `n` says. Used by tests that need to pin a
// specific count (e.g. drift by one) without depending on whatever
// the real enum currently has.
function syntheticProviderTypes(n) {
  const variants = Array.from({ length: n }, (_, i) => `v${i}`);
  return `export type Provider = ${variants.map((v) => `"${v}"`).join(' | ')};`;
}

// Build a README with the minimum required Features bullet. Variant
// labels passed in become the bullet's body. All other anchors
// (Windows 10/11, SmartScreen, data-dir, Get help) are pre-populated
// so tests can focus on the subject under test.
function fixtureReadme({ labels, countWord, countNumeral, dropFeatureLabels = [], extra = '' }) {
  const present = labels.filter((l) => !dropFeatureLabels.includes(l));
  const features = present.length > 0
    ? `- **${countWord ?? countNumeral} harnesses, one workflow**: ${present.join(', ')}.\n`
    : `- A bullet that does not mention harnesses.`;
  return [
    '# Buildmesh',
    '',
    '## Features',
    '',
    '### Multi-agent orchestration',
    features,
    '',
    '## Get help',
    '',
    'Ask in the Discussions tab.',
    '',
    'Windows 10/11 supported. SmartScreen prompt expected. Data lives under %APPDATA%\\com.alond.buildmesh.',
    extra,
  ].join('\n');
}

const labelsFromHarnessLabel = Object.values(realHarnessLabelMap);

test('passes against the REAL README + REAL Provider.ts + REAL HARNESS_LABEL', () => {
  // This is the gate's production input: the actual README + actual
  // generated binding + actual canonical label table. If a contributor
  // lands a PR that breaks the gate, this catches it without any
  // fixture plumbing.
  const { failures, variants } = checkReadmeDrift({
    readme: realReadme,
    providerTypes: realProviderTypes,
    harnessLabel: realHarnessLabelMap,
  });
  assert.equal(failures.length, 0, failures.join('\n'));
  assert.ok(variants.length > 0, 'real Provider enum parsed zero variants');
});

test('passes when the Features bullet mentions every HARNESS_LABEL entry', () => {
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
  });
  const { failures } = checkReadmeDrift({
    readme,
    providerTypes: realProviderTypes,
    harnessLabel: realHarnessLabelMap,
  });
  assert.equal(failures.length, 0, failures.join('\n'));
});

test('fails when a label is dropped from the Features bullet', () => {
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
    dropFeatureLabels: ['Cursor'],
  });
  const { failures } = checkReadmeDrift({
    readme,
    providerTypes: realProviderTypes,
    harnessLabel: realHarnessLabelMap,
  });
  assert.ok(
    failures.some((f) => /harness-label-mentioned.*cursor.*Cursor/.test(f)),
    `expected harness-label-mentioned for cursor, got: ${failures.join(' | ')}`
  );
});

test('does NOT false-pass when a label word appears in unrelated prose outside the Features bullet', () => {
  // The "Terminal" word must appear in the Features bullet. Putting
  // it ONLY in a sidebar paragraph under a different heading would
  // pass the unscoped check but should fail the scoped one.
  const labelsWithoutTerminal = labelsFromHarnessLabel.filter((l) => l !== 'Terminal');
  const readme = fixtureReadme({
    labels: labelsWithoutTerminal,
    countWord: numberToWord(labelsFromHarnessLabel.length),
    extra: '\nThe plain "Terminal" word shows up here, in the sidebar, but not in the Features bullet.',
  });
  const { failures } = checkReadmeDrift({
    readme,
    providerTypes: realProviderTypes,
    harnessLabel: realHarnessLabelMap,
  });
  assert.ok(
    failures.some((f) => /harness-label-mentioned.*terminal/.test(f)),
    `expected harness-label-mentioned for Terminal when its word only appears outside the Features bullet, got: ${failures.join(' | ')}`
  );
});

test('fails when HARNESS_LABEL has no entry for a variant (out-of-sync)', () => {
  // Synthetic 12-variant Provider enum where one variant is renamed
  // and the matching label entry is dropped. The count stays at 12
  // so the count-claim check stays focused on this test's subject.
  const providerTypes = realProviderTypes.replace('"cursor"', '"newprovider"');
  const harnessLabel = { ...realHarnessLabelMap };
  delete harnessLabel.cursor;
  const readme = fixtureReadme({
    labels: Object.values(harnessLabel),
    countWord: 'twelve',
  });
  const { failures } = checkReadmeDrift({ readme, providerTypes, harnessLabel });
  assert.ok(
    failures.some((f) => /harness-label-coverage.*newprovider/.test(f)),
    `expected harness-label-coverage failure, got: ${failures.join(' | ')}`
  );
});

test('fails when the README reverts to the stale Six providers claim', () => {
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel.slice(0, 6),
    countWord: 'Six',
  }) + '\n\nSix providers: Anthropic, Minimax, Kimi, OpenCode, Antigravity, Codex.';
  const { failures } = checkReadmeDrift({ readme, providerTypes: realProviderTypes, harnessLabel: realHarnessLabelMap });
  assert.ok(failures.some((f) => /no-stale-six/.test(f)));
});

test('fails when the README count claim drifts from the enum size', () => {
  // Add a 13th variant (with a matching label) but keep the README
  // saying "twelve". The count-claim check must trip.
  const providerTypes = realProviderTypes.replace(
    /"terminal";/,
    '"terminal" | "newprovider";'
  );
  const harnessLabel = { ...realHarnessLabelMap, newprovider: 'New Provider' };
  const readme = fixtureReadme({
    labels: Object.values(harnessLabel),
    countWord: 'twelve',
  });
  const { failures } = checkReadmeDrift({ readme, providerTypes, harnessLabel });
  assert.ok(
    failures.some((f) => /count-claim/.test(f)),
    `expected count-claim failure, got: ${failures.join(' | ')}`
  );
});

test('passes when the README count claim uses Arabic numerals', () => {
  // "12 harnesses" — numeral form, not the English word.
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countNumeral: '12',
  });
  const { failures } = checkReadmeDrift({
    readme,
    providerTypes: realProviderTypes,
    harnessLabel: realHarnessLabelMap,
  });
  assert.equal(failures.length, 0, failures.join('\n'));
});

test('fails when the README pastes a GitHub-issue cross-ref', () => {
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
    extra: '\n\nSee [#1234](https://github.com/alondero/buildmesh/issues/1234).',
  });
  const { failures } = checkReadmeDrift({ readme, providerTypes: realProviderTypes, harnessLabel: realHarnessLabelMap });
  assert.ok(failures.some((f) => /no-internal-issue-refs/.test(f)));
});

test('fails when the README uses parenthetical #NNN or ADR-NNNN prose', () => {
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
    extra: '\n\nThe pivot off AppContainer (#528) and the related ADR-0014 matter.',
  });
  const { failures } = checkReadmeDrift({ readme, providerTypes: realProviderTypes, harnessLabel: realHarnessLabelMap });
  assert.ok(failures.some((f) => /no-internal-issue-refs/.test(f)));
});

test('fails on bare #NNN prose (no surrounding parens or prefix words)', () => {
  // Real bare prose: `macOS Seatbelt #497 ...` — the #497 isn't
  // wrapped in parens or preceded by "see" / "fixed in" / etc.
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
    extra: '\n\nmacOS Seatbelt #497 and the loopback fix #533.',
  });
  const { failures } = checkReadmeDrift({ readme, providerTypes: realProviderTypes, harnessLabel: realHarnessLabelMap });
  assert.ok(failures.some((f) => /no-internal-issue-refs/.test(f)));
});

test('does NOT double-count the same issue caught by overlapping patterns', () => {
  // (issue #1234) trips THREE patterns at once: parenthetical-issue,
  // bare-#1234, and bare-#1234-within-parens. The dedup should
  // collapse them to a single entry so the failure message is one
  // issue, not three.
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
    extra: '\n\nThis (issue #1234) cross-refers to see #1234 elsewhere.',
  });
  const { failures } = checkReadmeDrift({ readme, providerTypes: realProviderTypes, harnessLabel: realHarnessLabelMap });
  const issueFailure = failures.find((f) => /no-internal-issue-refs/.test(f));
  assert.ok(issueFailure, 'expected expected-issue-ref failure');
  // The message should reference "1 internal issue reference(s)",
  // not three.
  assert.match(issueFailure, /1 internal issue reference/);
});

test('catches 5-digit issue numbers (issue #10000+)', () => {
  // The previous regex was \d{3,4} and missed 5-digit refs.
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
    extra: '\n\nSee [#12345](https://github.com/alondero/buildmesh/issues/12345).',
  });
  const { failures } = checkReadmeDrift({ readme, providerTypes: realProviderTypes, harnessLabel: realHarnessLabelMap });
  assert.ok(failures.some((f) => /no-internal-issue-refs.*12345/.test(f)));
});

test('catches small issue numbers like (#7)', () => {
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
    extra: '\n\nTracked as (#7).',
  });
  const { failures } = checkReadmeDrift({ readme, providerTypes: realProviderTypes, harnessLabel: realHarnessLabelMap });
  assert.ok(failures.some((f) => /no-internal-issue-refs.*#7/.test(f)));
});

test('fails when the Get help heading is renamed (substring trap regression)', () => {
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
  }).replace('## Get help', '## Need help');
  const { failures } = checkReadmeDrift({ readme, providerTypes: realProviderTypes, harnessLabel: realHarnessLabelMap });
  assert.ok(failures.some((f) => /get-help-heading/.test(f)));
});

test('accepts Get help at any heading level', () => {
  const readme = [
    '# Buildmesh',
    '',
    '## Install',
    '',
    fixtureReadme({
      labels: labelsFromHarnessLabel,
      countWord: numberToWord(labelsFromHarnessLabel.length),
    }),
    '',
    '### Get help',
    '',
    'Ask in the Discussions tab.',
  ].join('\n');
  const { failures } = checkReadmeDrift({ readme, providerTypes: realProviderTypes, harnessLabel: realHarnessLabelMap });
  assert.equal(failures.length, 0, failures.join('\n'));
});

test('fails when the Multi-agent orchestration section is missing', () => {
  // Replace the section with nothing — the scoped checks must
  // surface a clear "features section missing" failure rather than
  // silently passing because the unscoped reads happen to find the
  // labels elsewhere in the doc.
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
  }).replace(/### Multi-agent orchestration[\s\S]*?(?=\n## )/m, '');
  const { failures } = checkReadmeDrift({ readme, providerTypes: realProviderTypes, harnessLabel: realHarnessLabelMap });
  assert.ok(failures.some((f) => /features-multi-agent-section/.test(f)));
});

test('fails when the Provider union is unparseable', () => {
  const readme = fixtureReadme({
    labels: labelsFromHarnessLabel,
    countWord: numberToWord(labelsFromHarnessLabel.length),
  });
  const { failures, variants } = checkReadmeDrift({
    readme,
    providerTypes: '// file moved, shape changed\n',
    harnessLabel: realHarnessLabelMap,
  });
  assert.equal(variants.length, 0);
  assert.ok(failures.some((f) => /provider-binding/.test(f)));
});

test('parseProviderVariants reads only the union body, not docstring tokens', () => {
  const src = `// The /"thing"/ annotation in this docstring must NOT count as a variant.\n/** @example \`export type X = "foo"\` */\nexport type Provider = "anthropic" | "agy";\n`;
  assert.deepEqual(parseProviderVariants(src), ['anthropic', 'agy']);
});

test('parseHarnessLabel extracts the canonical table', () => {
  const out = _parseHarnessLabel(realHarnessLabelTs);
  assert.ok(out, 'parseHarnessLabel returned null');
  assert.equal(out.anthropic, 'Claude Code');
  assert.equal(out.codex, 'Codex');
  assert.equal(out.kimi, 'Kimi Code');
  assert.equal(out.grok, 'Grok Code');
  // Don't pin the exact count — a new harness added to the catalog
  // should pass this test, not break it. Just assert non-empty and
  // that every value is a non-empty string.
  assert.ok(Object.keys(out).length > 0);
  for (const v of Object.values(out)) {
    assert.ok(typeof v === 'string' && v.length > 0, `bad label value: ${v}`);
  }
});

test('featuresMultiAgentSection extracts the Features bullet', () => {
  const section = featuresMultiAgentSection(realReadme);
  assert.ok(section, 'real README did not yield a Features section');
  // The section starts with the bullet and contains at least one label.
  assert.match(section, /^- /);
  assert.match(section, /Claude Code/);
});

test('numberToWord covers the supported range', () => {
  assert.equal(numberToWord(0), 'zero');
  assert.equal(numberToWord(2), 'two');
  assert.equal(numberToWord(12), 'twelve');
  assert.equal(numberToWord(13), 'thirteen');
  assert.equal(numberToWord(24), 'twenty-four');
  assert.equal(numberToWord(25), null);
  assert.equal(numberToWord(-1), null);
  assert.equal(numberToWord(1.5), null);
});