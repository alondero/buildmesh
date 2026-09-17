import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
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
