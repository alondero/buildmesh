// Gate tests for the dependency and workflow supply-chain controls (issue #1541).
//
// These test the *policy* in each gate, not the current repository state. The
// negative fixtures are the point: a gate that has only ever been run against a
// clean tree is a gate nobody has watched reject anything, so each test asserts a
// deliberately vulnerable or unpinned input actually fails, with a message that
// names the offending dependency.
//
// Run with: node --test tests/agent-infra/supply-chain-gates.test.mjs

import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  checkActionFile,
  collectReferences,
  parseUses,
  verifyShas,
  FULL_SHA,
} from '../../scripts/check-action-pins.mjs';
import { evaluateEntries, evaluateReports, meetsThreshold, SEVERITIES } from '../../scripts/check-npm-audit.mjs';
import {
  evaluateFindings,
  loadExceptions,
  parseAuditReport,
  validateExceptions,
  BLOCKING_KINDS,
} from '../../scripts/check-rust-advisories.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');

const SHA = '0123456789abcdef0123456789abcdef01234567';
const FUTURE = '2999-01-01';
const PAST = '2000-01-01';

// ---------------------------------------------------------------- action pins

test('action pins: a mutable tag reference is rejected', () => {
  const violations = checkActionFile('      - uses: actions/checkout@v4\n');
  assert.equal(violations.length, 1, 'a tag-pinned action must fail the gate');
  assert.match(violations[0], /mutable ref `v4`/);
  assert.match(violations[0], /commit SHA/);
});

test('action pins: a branch reference is rejected', () => {
  // This is the real supply-chain hole: a branch reference is mutable by
  // definition, and pinning to one is a pin that can move under a merge.
  const violations = checkActionFile('      - uses: actions/checkout@main\n');
  assert.equal(violations.length, 1);
  assert.match(violations[0], /mutable ref `main`/);
});

test('action pins: a full SHA with a version comment passes', () => {
  const violations = checkActionFile(`      - uses: actions/checkout@${SHA} # v4\n`);
  assert.deepEqual(violations, []);
});

test('action pins: a truncated SHA is rejected, not silently accepted', () => {
  const violations = checkActionFile(`      - uses: actions/checkout@${SHA.slice(0, 12)} # v4\n`);
  assert.equal(violations.length, 1);
  assert.match(violations[0], /mutable ref/);
});

test('action pins: an uppercase SHA is rejected (GitHub refs are lowercase)', () => {
  const violations = checkActionFile(`      - uses: actions/checkout@${SHA.toUpperCase()} # v4\n`);
  assert.equal(violations.length, 1);
});

test('action pins: a SHA without a version comment is rejected', () => {
  const violations = checkActionFile(`      - uses: actions/checkout@${SHA}\n`);
  assert.equal(violations.length, 1);
  assert.match(violations[0], /no version comment/);
});

test('action pins: local and docker references are exempt', () => {
  const markdown = [
    '      - uses: ./.github/actions/install-linux-build-deps',
    '      - uses: ./.github/workflows/verify.yml',
    '      - uses: docker://alpine:3.20',
    '',
  ].join('\n');
  assert.deepEqual(checkActionFile(markdown), []);
});

test('action pins: a commented-out unpinned reference does not fail the gate', () => {
  // Workflow prose legitimately quotes the old tag; enforcing on it would make
  // the gate impossible to satisfy while documenting history.
  const markdown = [
    '# Historically this used `uses: actions/checkout@v3` (now SHA-pinned below).',
    '      # - uses: actions/checkout@v3',
    `      - uses: actions/checkout@${SHA} # v7`,
    '',
  ].join('\n');
  assert.deepEqual(checkActionFile(markdown), []);
});

test('action pins: a third-party reusable workflow is pinned too', () => {
  const pinned = `  uses: someorg/shared/.github/workflows/build.yml@${SHA} # v2`;
  assert.deepEqual(checkActionFile(pinned), []);
  assert.equal(checkActionFile('  uses: someorg/shared/.github/workflows/build.yml@v2\n').length, 1);
});

test('action pins: an allowlisted reference is skipped, others still checked', () => {
  const markdown = '      - uses: acme/no-tags-action@main\n';
  assert.equal(checkActionFile(markdown).length, 1);
  assert.deepEqual(checkActionFile(markdown, { allow: new Set(['acme/no-tags-action@main']) }), []);
});

test('action pins: parseUses keeps the line number so failures are locatable', () => {
  const markdown = 'steps:\n  - uses: a/b@v1\n';
  const found = parseUses(markdown);
  assert.equal(found.length, 1);
  assert.equal(found[0].line, 2);
  assert.equal(found[0].ref, 'a/b@v1');
});

test('action pins: this repository pins every third-party action', () => {
  // The invariant the gate exists to protect, asserted against the real tree so
  // a future unpinned action cannot land without this test noticing too.
  const files = [
    ...fs.readdirSync(path.join(repoRoot, '.github', 'workflows'))
      .filter((f) => /\.ya?ml$/.test(f))
      .map((f) => path.join(repoRoot, '.github', 'workflows', f)),
    path.join(repoRoot, '.github', 'actions', 'install-linux-build-deps', 'action.yml'),
  ];
  for (const file of files) {
    const violations = checkActionFile(fs.readFileSync(file, 'utf8'), {
      file: path.relative(repoRoot, file),
    });
    assert.deepEqual(violations, [], `${path.basename(file)} has unpinned third-party actions`);
  }
  const references = collectReferences(repoRoot);
  assert.ok(references.length > 0, 'expected the repository to reference third-party actions');
  for (const reference of references) {
    assert.match(reference.sha, FULL_SHA, `${reference.key} is not a full commit SHA`);
  }
});

test('action pins: verifyShas reports a SHA the API does not resolve', () => {
  // A typo or a truncated SHA must fail here rather than at run time.
  const fetchJson = async () => ({ sha: 'someothersha' });
  return verifyShas([{ owner: 'acme/action', sha: SHA }], { fetchJson }).then((problems) => {
    assert.equal(problems.length, 1);
    assert.match(problems[0], /did not resolve/);
  });
});

test('action pins: verifyShas reports an API failure without throwing', () => {
  // A transient network failure must not read as "the repository is unpinned",
  // and must not crash the gate either.
  const fetchJson = async () => {
    throw new Error('HTTP 503');
  };
  return verifyShas([{ owner: 'acme/action', sha: SHA }], { fetchJson }).then((problems) => {
    assert.equal(problems.length, 1);
    assert.match(problems[0], /could not verify \(HTTP 503\)/);
  });
});

// ----------------------------------------------------------------- npm audit

test('npm audit: severity ordering is the npm order, not alphabetical', () => {
  assert.ok(SEVERITIES.indexOf('critical') > SEVERITIES.indexOf('moderate'));
  assert.ok(meetsThreshold('high', 'high'));
  assert.ok(!meetsThreshold('moderate', 'high'));
  assert.ok(meetsThreshold('critical', 'low'));
});

// The fixture below is real `npm audit --json` output, captured from a throwaway
// project with a vulnerable production dependency (lodash 4.17.15) and a
// vulnerable dev-only one (minimist 0.0.8). It replaced hand-written fixtures
// that asserted a `dev` flag npm does not emit at all — those passed while the
// gate silently treated every advisory as production.
const vulnerableFixture = JSON.parse(
  fs.readFileSync(path.join(repoRoot, 'tests', 'fixtures', 'npm-audit-vulnerable.json'), 'utf8'),
);

test('npm audit: the captured fixture really does span both scopes', () => {
  // If a future npm stopped reporting a package under --omit=dev, the fixture
  // would stop exercising the dev branch and the tiering tests below would pass
  // vacuously. This asserts the fixture still has the shape it was captured for.
  const full = Object.keys(vulnerableFixture.full.vulnerabilities);
  const production = Object.keys(vulnerableFixture.production.vulnerabilities);
  assert.ok(full.includes('lodash') && production.includes('lodash'), 'lodash must be production');
  assert.ok(full.includes('minimist') && !production.includes('minimist'), 'minimist must be dev-only');
});

test('npm audit: a production high-severity advisory fails the gate', () => {
  const { recognised, failures } = evaluateReports(vulnerableFixture.full, vulnerableFixture.production);
  assert.ok(recognised);
  const lodash = failures.find((failure) => failure.name === 'lodash');
  assert.ok(lodash, 'the production advisory must be reported');
  assert.equal(lodash.scope, 'production');
  assert.ok(lodash.titles.length > 0, 'the advisory title is surfaced, not just the name');
});

test('npm audit: a dev-only advisory is classified dev from the two-report difference', () => {
  // This is the assertion that would have caught the bug: minimist is dev-only,
  // and the only way to know that is that `audit --omit=dev` did not report it.
  const { devOnlyNames } = evaluateReports(vulnerableFixture.full, vulnerableFixture.production);
  assert.ok(devOnlyNames.has('minimist'));
  assert.ok(!devOnlyNames.has('lodash'));
});

test('npm audit: a dev-only moderate advisory is reported but does not fail', () => {
  // The whole point of the two-tier policy: dev-only exposure differs from
  // shipped runtime exposure, so it must not block a merge. Lower the captured
  // dev-only severity to `moderate` and it must fall below the dev threshold.
  const full = structuredClone(vulnerableFixture.full);
  const production = structuredClone(vulnerableFixture.production);
  full.vulnerabilities.minimist.severity = 'moderate';
  const { failures } = evaluateReports(full, production);
  assert.equal(failures.filter((failure) => failure.name === 'minimist').length, 0);
  // It is still classified dev-only, so this is "reported, not failed" rather
  // than "silently dropped".
  assert.equal(failures.filter((failure) => failure.name === 'lodash').length, 1);
});

test('npm audit: a dev-only high advisory does fail', () => {
  // Dev tooling executes attacker-influenced input (install scripts, parsers fed
  // crafted fixtures), so dev-only high is still treated as a failure.
  const full = structuredClone(vulnerableFixture.full);
  const production = structuredClone(vulnerableFixture.production);
  full.vulnerabilities.minimist.severity = 'high';
  const { failures } = evaluateReports(full, production);
  const minimist = failures.find((failure) => failure.name === 'minimist');
  assert.ok(minimist, 'a dev-only high advisory must fail');
  assert.equal(minimist.scope, 'dev');
});

test('npm audit: production-only mode cannot misclassify a dev advisory as dev-only', () => {
  // With `--omit=dev` npm has already excluded dev dependencies, so nothing may
  // be classified as dev-only and the stricter production threshold applies.
  const { devOnlyNames, failures } = evaluateReports(vulnerableFixture.production, null);
  assert.deepEqual([...(devOnlyNames ?? [])], []);
  assert.equal(failures.length, 1, 'only the production advisory is present at all');
});

test('npm audit: the npm 6 advisories-map shape is also understood', () => {
  const report = {
    advisories: {
      42: { module_name: 'lodash', severity: 'critical', is_direct: true, vulnerable_versions: '<4.17.21' },
    },
  };
  const { recognised, failures } = evaluateReports(report, null);
  assert.ok(recognised);
  assert.equal(failures.length, 1);
  assert.equal(failures[0].name, 'lodash');
});

test('npm audit: an unrecognised report shape is not a clean bill of health', () => {
  // The important negative: if npm changes its output, the gate must fail loudly
  // rather than read zero advisories in a format it does not understand.
  const { recognised, failures } = evaluateReports({ metadata: { vulnerabilities: 0 } }, null);
  assert.equal(recognised, false);
  assert.equal(failures.length, 0);
});

test('npm audit: a package npm could not classify is held to the production threshold', () => {
  // When nothing marks a package dev-only, the conservative assumption is that
  // it ships. A moderate advisory must fail rather than slip through on the
  // dev threshold.
  const entries = [{ name: 'mystery', severity: 'moderate', direct: true, titles: ['x'], range: '' }];
  assert.equal(evaluateEntries(entries).length, 1);
  assert.equal(evaluateEntries(entries, { devOnlyNames: new Set(['mystery']) }).length, 0);
});

test('npm audit: production dependencies are clean at this commit', () => {
  const lock = JSON.parse(fs.readFileSync(path.join(repoRoot, 'package-lock.json'), 'utf8'));
  const devOnly = Object.entries(lock.packages ?? {}).filter(([, entry]) => entry.dev === true);
  // Sanity-check the fixture the production gate depends on: the advisory that
  // issue #1541 recorded was dev-only, and stayed dev-only.
  assert.ok(devOnly.length > 0, 'expected dev-only packages in the lockfile');
  const knownFixed = ['brace-expansion', 'source-map-js', 'undici'];
  for (const name of knownFixed) {
    const entry = lock.packages[`node_modules/${name}`];
    assert.ok(entry, `${name} should still be in the lockfile`);
    assert.equal(entry.dev, true, `${name} must remain dev-only`);
  }
});

// ------------------------------------------------------------ Rust advisories

test('rust advisories: parsing flattens grouped warnings and vulnerabilities', () => {
  const report = {
    vulnerabilities: {
      list: [
        {
          advisory: { id: 'RUSTSEC-2026-0285', title: 'TLS handshake accepted incorrectly' },
          package: { name: 'rustls', version: '0.23.43' },
        },
      ],
    },
    warnings: {
      unmaintained: [
        { advisory: { id: 'RUSTSEC-2025-0141' }, package: { name: 'bincode', version: '1.3.3' } },
      ],
      // A yanked crate has no RustSec advisory document at all.
      yanked: [{ package: { name: 'libssh2-sys', version: '0.3.2' } }],
    },
  };
  const findings = parseAuditReport(report);
  assert.equal(findings.length, 3);
  assert.deepEqual(
    findings.map((f) => `${f.kind}:${f.crate}`),
    ['vulnerability:rustls', 'unmaintained:bincode', 'yanked:libssh2-sys'],
  );
  // The yanked entry has no advisory, so it must still be matchable by crate.
  assert.equal(findings[2].id, null);
});

test('rust advisories: a vulnerability always fails, even with an exception listed', () => {
  // This is the invariant that makes the policy worth having: a reviewed
  // exception must never be able to excuse a real vulnerability.
  const entries = [
    {
      id: 'RUSTSEC-2026-0285',
      crate: 'rustls',
      version: '0.23.43',
      owner: 'maintainers',
      rationale: 'we looked at it',
      reviewBy: FUTURE,
    },
  ];
  const findings = [
    {
      id: 'RUSTSEC-2026-0285',
      crate: 'rustls',
      version: '0.23.43',
      kind: 'vulnerability',
      title: 'TLS 1.3 handshake messages incorrectly accepted',
    },
  ];
  const { failures } = evaluateFindings(findings, entries);
  assert.equal(failures.length, 1);
  assert.equal(failures[0].reason, 'vulnerability');
  assert.ok(BLOCKING_KINDS.includes('vulnerability'));
});

test('rust advisories: an unreviewed warning fails as unreviewed', () => {
  const findings = [
    { id: 'RUSTSEC-2099-0001', crate: 'mystery', version: '1.0.0', kind: 'unmaintained', title: '' },
  ];
  const { failures } = evaluateFindings(findings, []);
  assert.equal(failures.length, 1);
  assert.equal(failures[0].reason, 'unreviewed');
  assert.match(failures[0].detail, /no reviewed exception/);
});

test('rust advisories: a reviewed, unexpired exception passes', () => {
  const entries = [
    {
      id: 'RUSTSEC-2025-0141',
      crate: 'bincode',
      version: '1.3.3',
      owner: 'maintainers',
      rationale: 'transitive through Tauri',
      reviewBy: FUTURE,
    },
  ];
  const findings = [
    { id: 'RUSTSEC-2025-0141', crate: 'bincode', version: '1.3.3', kind: 'unmaintained', title: '' },
  ];
  const { failures, stale } = evaluateFindings(findings, entries);
  assert.deepEqual(failures, []);
  assert.deepEqual(stale, []);
});

test('rust advisories: an expired exception fails, so it cannot persist by neglect', () => {
  const entries = [
    {
      id: 'RUSTSEC-2025-0141',
      crate: 'bincode',
      version: '1.3.3',
      owner: 'maintainers',
      rationale: 'reviewed once in 2025',
      reviewBy: PAST,
    },
  ];
  const findings = [
    { id: 'RUSTSEC-2025-0141', crate: 'bincode', version: '1.3.3', kind: 'unmaintained', title: '' },
  ];
  const { failures } = evaluateFindings(findings, entries);
  assert.equal(failures.length, 1);
  assert.equal(failures[0].reason, 'expired');
});

test('rust advisories: a yanked crate with no advisory id matches by crate@version', () => {
  const entries = [
    {
      crate: 'libssh2-sys',
      version: '0.3.2',
      owner: 'maintainers',
      rationale: 'yanked upstream, no known advisory',
      reviewBy: FUTURE,
    },
  ];
  const findings = [
    { id: null, crate: 'libssh2-sys', version: '0.3.2', kind: 'yanked', title: '' },
  ];
  const { failures, stale } = evaluateFindings(findings, entries);
  assert.deepEqual(failures, []);
  assert.deepEqual(stale, [], 'a matched yanked entry is not stale');
});

test('rust advisories: an exception for an advisory that no longer exists is stale', () => {
  // The crate was fixed or removed, so the rationale describes a tree that is
  // no longer there and should be deleted rather than left to rot.
  const entries = [
    {
      id: 'RUSTSEC-2020-0001',
      crate: 'old-crate',
      version: '1.0.0',
      owner: 'maintainers',
      rationale: 'transitive',
      reviewBy: FUTURE,
    },
  ];
  const { stale } = evaluateFindings([], entries);
  assert.equal(stale.length, 1);
  assert.equal(stale[0].key, 'RUSTSEC-2020-0001');
});

test('rust advisories: an exception missing owner, rationale or reviewBy is not reviewable', () => {
  const entries = [
    { id: 'RUSTSEC-2025-0141', crate: 'bincode', version: '1.3.3', rationale: 'no owner' },
    { id: 'RUSTSEC-2025-0142', crate: 'other', version: '1.0.0', owner: 'maintainers' },
  ];
  const problems = validateExceptions(entries);
  assert.ok(problems.some((p) => /missing an owner/.test(p)));
  assert.ok(problems.some((p) => /missing a rationale/.test(p)));
  assert.ok(problems.some((p) => /missing a reviewBy/.test(p)));
});

test('rust advisories: a duplicate exception entry is rejected', () => {
  const entry = {
    id: 'RUSTSEC-2025-0141',
    crate: 'bincode',
    version: '1.3.3',
    owner: 'maintainers',
    rationale: 'transitive',
    reviewBy: FUTURE,
  };
  const problems = validateExceptions([entry, { ...entry }]);
  assert.ok(problems.some((p) => /listed twice/.test(p)));
});

test('rust advisories: the committed exception file is complete and in date', () => {
  // Every exception this repository actually ships must carry the three fields
  // the policy requires, and none may be past its review date.
  const { entries } = loadExceptions(path.join(repoRoot, '.github', 'rustsec-exceptions.json'));
  assert.ok(entries.length > 0, 'expected reviewed exceptions');
  assert.deepEqual(validateExceptions(entries, { now: new Date() }), []);
  for (const entry of entries) {
    assert.ok(entry.owner && entry.rationale.length > 20, `${entry.id} needs a substantive rationale`);
  }
});