import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  ALWAYS_LOADED_DOC_BUDGETS,
  checkAlwaysLoadedBudgets,
  checkBacktickedRepoPaths,
  checkDocumentation,
  checkDocumentationImpact,
  checkLocalLinks,
  changedFilesSince,
  collectMarkdownFiles,
  extractMarkdownLinks,
  githubAnchor,
  hasDocumentStatus,
  pathExistsExactly,
  stripFencedCode,
} from '../../scripts/check-docs.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));

test('the documentation contract passes for the real repository', () => {
  const failures = checkDocumentation({ root });
  assert.deepEqual(failures, [], failures.join('\n'));
  assert.ok(collectMarkdownFiles(root).includes(join(root, 'CONTEXT.md')));
  assert.ok(collectMarkdownFiles(root).includes(join(root, 'docs', 'releases', 'v1.3.0.md')));
});

test('local links check both targets and GitHub-style anchors', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-docs-'));
  try {
    mkdirSync(join(fixtureRoot, 'docs'));
    const source = join(fixtureRoot, 'docs', 'source.md');
    const target = join(fixtureRoot, 'docs', 'target.md');
    const image = join(fixtureRoot, 'docs', 'image.png');
    writeFileSync(target, '# A real heading\n');
    writeFileSync(image, 'not really an image, but the path is enough for this check');
    writeFileSync(source, [
      '# Source',
      '',
      '[same page](#source)',
      '[good](target.md#a-real-heading)',
      '[bad target](missing.md)',
      '[bad anchor](target.md#not-real)',
      '![](image.png)',
    ].join('\n'));

    const failures = checkLocalLinks({ root: fixtureRoot, files: [source] });
    assert.equal(failures.length, 3);
    assert.match(failures[0], /missing\.md/);
    assert.match(failures[1], /#not-real/);
    assert.match(failures[2], /empty alt text/);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('link extraction skips external URLs and preserves image metadata', () => {
  const links = extractMarkdownLinks([
    '[remote](https://example.com)',
    '![logo](../src/assets/wordmark.png)',
    '[local](guide.md "read this")',
  ].join('\n'));
  assert.deepEqual(links, [
    { target: 'https://example.com', image: false, alt: 'remote' },
    { target: '../src/assets/wordmark.png', image: true, alt: 'logo' },
    { target: 'guide.md', image: false, alt: 'local' },
  ]);
});

test('anchor normalisation matches common Markdown headings', () => {
  assert.equal(githubAnchor('Remote access: HTTPS/WSS & pairing'), 'remote-access-httpswss--pairing');
  assert.equal(githubAnchor('  A heading  '), 'a-heading');
});

test('heading and status checks ignore fenced examples but enforce document metadata', () => {
  assert.equal(stripFencedCode('# Real\n```\n# Example\n```\n').includes('# Example'), false);
  assert.equal(hasDocumentStatus('# Doc\n\nStatus: accepted\n'), true);
  assert.equal(hasDocumentStatus('# Doc\n\n## Status\n\nAccepted\n'), true);
  assert.equal(hasDocumentStatus('# Doc\n'), false);
});

test('versioned release notes use a matching Buildmesh title', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-release-notes-'));
  try {
    mkdirSync(join(fixtureRoot, 'docs', 'releases'), { recursive: true });
    const release = join(fixtureRoot, 'docs', 'releases', 'v2.0.0.md');
    writeFileSync(release, '# Buildmesh v1.9.0\n');
    const failures = checkDocumentation({ root: fixtureRoot, files: [release] });
    assert.ok(failures.some((failure) => failure.includes('docs/releases/v2.0.0.md must have the title')));
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('documentation impact requires a relevant page or a reasoned exemption', () => {
  assert.equal(checkDocumentationImpact({ changedFiles: ['src/App.tsx'] }).length, 1);
  assert.equal(checkDocumentationImpact({
    changedFiles: ['src/App.tsx'],
    commitMessages: ['feat: change app\n\ndocs: none — generated fixture only'],
  }).length, 0);
  assert.deepEqual(checkDocumentationImpact({
    changedFiles: ['src/App.tsx', 'docs/user-guide.md'],
  }), []);
  assert.deepEqual(checkDocumentationImpact({ changedFiles: ['tests/unit/app.test.tsx'] }), []);
});

test('documentation impact base handling tolerates first-push and unavailable revisions', () => {
  const firstPush = changedFilesSince(root, '0'.repeat(40));
  assert.equal(firstPush.skipped, false);
  assert.equal(firstPush.base, 'HEAD');

  const unavailable = changedFilesSince(root, 'revision-that-does-not-exist');
  assert.equal(unavailable.skipped, true);
  assert.deepEqual(unavailable.changedFiles, []);
  assert.deepEqual(unavailable.commitMessages, []);
});

test('local path validation is case-sensitive even on Windows', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-doc-case-'));
  try {
    mkdirSync(join(fixtureRoot, 'docs'));
    writeFileSync(join(fixtureRoot, 'docs', 'target.md'), '# Target\n');
    assert.equal(pathExistsExactly(fixtureRoot, join(fixtureRoot, 'docs', 'target.md')), true);
    assert.equal(pathExistsExactly(fixtureRoot, join(fixtureRoot, 'docs', 'Target.md')), false);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('the real user guide is sourced from the current harness catalog', () => {
  const guide = readFileSync(join(root, 'docs', 'user-guide.md'), 'utf8');
  assert.match(guide, /\| Claude Code \|/);
  assert.match(guide, /\| Terminal \| No \|/);
  assert.match(guide, /\| Meta Muse \| Yes \|/);
});

test('always-loaded documents stay inside their read-cost budget', () => {
  // The primer was 166 KB before issue #2045 split it into owner docs; the
  // budget is what stops it quietly growing back into a manual.
  assert.deepEqual(checkAlwaysLoadedBudgets({ root }), []);
  assert.ok(
    ALWAYS_LOADED_DOC_BUDGETS['docs/knowledge-primer.md'] < 32 * 1024,
    'the primer budget must stay far below the size it was split from',
  );
});

test('an always-loaded document over budget fails with the size and the remedy', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-doc-budget-'));
  try {
    mkdirSync(join(fixtureRoot, 'docs'), { recursive: true });
    writeFileSync(join(fixtureRoot, 'docs', 'primer.md'), `# Primer\n\n${'x'.repeat(500)}\n`);
    const failures = checkAlwaysLoadedBudgets({
      root: fixtureRoot,
      budgets: { 'docs/primer.md': 100 },
    });
    assert.equal(failures.length, 1);
    assert.match(failures[0], /docs\/primer\.md is 0\.5 KB, over its 0\.1 KB budget/);
    assert.match(failures[0], /Move the detail into the owner doc/);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('a missing always-loaded document is reported rather than skipped', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-doc-budget-missing-'));
  try {
    const failures = checkAlwaysLoadedBudgets({
      root: fixtureRoot,
      budgets: { 'docs/absent.md': 1024 },
    });
    assert.equal(failures.length, 1);
    assert.match(failures[0], /is missing: docs\/absent\.md/);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('the documented budgets cover the documents every agent loads first', () => {
  for (const path of ['CLAUDE.md', 'CONTEXT.md', 'docs/agents/engineering.md', 'docs/knowledge-primer.md']) {
    assert.ok(path in ALWAYS_LOADED_DOC_BUDGETS, `${path} must have a declared budget`);
    assert.ok(existsSync(join(root, path)), `${path} must exist`);
  }
});

test('documents outside the archive declare a status; archived history is exempt', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-doc-status-'));
  try {
    mkdirSync(join(fixtureRoot, 'docs', 'development'), { recursive: true });
    mkdirSync(join(fixtureRoot, 'docs', 'archive', '2026-09'), { recursive: true });
    const contract = join(fixtureRoot, 'docs', 'development', 'contract.md');
    const archived = join(fixtureRoot, 'docs', 'archive', '2026-09', 'write-up.md');
    const readme = join(fixtureRoot, 'docs', 'development', 'README.md');
    writeFileSync(contract, '# Contract\n\nNo status here.\n');
    writeFileSync(archived, '# Write-up\n\nNo status here either.\n');
    writeFileSync(readme, '# Development\n\nA navigation page.\n');

    const failures = checkDocumentation({
      root: fixtureRoot,
      files: [contract, archived, readme],
    });
    const statusFailures = failures.filter((failure) => failure.includes('[document-status]'));
    assert.equal(statusFailures.length, 1, statusFailures.join('\n'));
    assert.match(statusFailures[0], /docs\/development\/contract\.md/);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('the primer routes each area to an owner document that exists', () => {
  const primer = readFileSync(join(root, 'docs', 'knowledge-primer.md'), 'utf8');
  const rows = [...primer.matchAll(/^\| (?!Area)(.+?) \| \[([\w.-]+\.md)\]\(development\/([\w.-]+\.md)\) \|/gm)];
  assert.ok(rows.length >= 10, `expected the index to route every area, found ${rows.length} rows`);
  for (const [, area, , target] of rows) {
    assert.ok(
      existsSync(join(root, 'docs', 'development', target)),
      `primer routes "${area.trim()}" to development/${target}, which does not exist`,
    );
  }
});

test('a backticked pointer to a moved document is reported with its new home', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-stale-pointer-'));
  try {
    mkdirSync(join(fixtureRoot, 'docs', 'development'), { recursive: true });
    mkdirSync(join(fixtureRoot, 'docs', 'archive', '2026-09'), { recursive: true });
    writeFileSync(join(fixtureRoot, 'docs', 'archive', '2026-09', 'write-up.md'), '# Write-up\n');
    const source = join(fixtureRoot, 'docs', 'development', 'contract.md');
    writeFileSync(source, '# Contract\n\nSee `docs/development/write-up.md` for history.\n');

    const failures = checkBacktickedRepoPaths({ root: fixtureRoot, files: [source] });
    assert.equal(failures.length, 1, failures.join('\n'));
    assert.match(failures[0], /backticked path "docs\/development\/write-up\.md" no longer exists/);
    assert.match(failures[0], /that file is now docs\/archive\/2026-09\/write-up\.md/);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('a backticked path that exists nowhere is left to prose, not flagged', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-placeholder-path-'));
  try {
    mkdirSync(join(fixtureRoot, 'docs', 'development'), { recursive: true });
    // Releasing procedures use the next version number as an illustrative
    // placeholder; no such file exists, and that is not a stale pointer.
    const source = join(fixtureRoot, 'docs', 'development', 'releasing.md');
    writeFileSync(source, '# Releasing\n\nThis writes `docs/releases/v9.9.9.md`.\n');
    assert.deepEqual(checkBacktickedRepoPaths({ root: fixtureRoot, files: [source] }), []);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('archived records may name paths as they were when written', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-archived-paths-'));
  try {
    mkdirSync(join(fixtureRoot, 'docs', 'archive', '2026-09'), { recursive: true });
    mkdirSync(join(fixtureRoot, 'docs', 'development'), { recursive: true });
    writeFileSync(join(fixtureRoot, 'docs', 'development', 'write-up.md'), '# Write-up\n');
    const archived = join(fixtureRoot, 'docs', 'archive', '2026-09', 'history.md');
    writeFileSync(archived, '# History\n\nThen it lived at `docs/development/write-up.md`.\n');
    assert.deepEqual(checkBacktickedRepoPaths({ root: fixtureRoot, files: [archived] }), []);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('the real repository has no stale backticked pointers left by the split', () => {
  assert.deepEqual(checkBacktickedRepoPaths({ root }), []);
});
