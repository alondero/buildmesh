import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import {
  compareVersions,
  groupCommits,
  isInternal,
  parseConventionalCommit,
  renderReleaseNotes,
  resolveBase,
} from '../../scripts/release-notes.mjs';

test('parses conventional commit subjects, scopes, breaking markers, and PR numbers', () => {
  assert.deepEqual(parseConventionalCommit('feat(spawn): add recipe (#12)'), {
    type: 'feat',
    scope: 'spawn',
    breaking: false,
    subject: 'add recipe',
    pr: '12',
  });
  assert.equal(parseConventionalCommit('fix: correct badge').type, 'fix');
  assert.equal(parseConventionalCommit('fix: correct badge').scope, null);
  assert.equal(parseConventionalCommit('fix: correct badge').pr, null);
  assert.equal(parseConventionalCommit('feat!: drop legacy mode').breaking, true);
  assert.equal(parseConventionalCommit('fix(api): handle nulls', 'Closes #99').pr, '99');
  assert.equal(parseConventionalCommit('chore(deps): bump', 'BREAKING CHANGE: removed').breaking, true);
});

test('collapses stacked PR numbers and keeps the landing one', () => {
  // Stacked PRs squash-merge with every number in the subject; the last is the
  // PR that actually landed, and the rest must not leak into the entry text.
  assert.deepEqual(parseConventionalCommit('feat(sidebar): sort meshes into a top band (#1939) (#1940)'), {
    type: 'feat',
    scope: 'sidebar',
    breaking: false,
    subject: 'sort meshes into a top band',
    pr: '1940',
  });
  assert.equal(parseConventionalCommit('feat: three (#1) (#2) (#3)').pr, '3');
  const draft = renderReleaseNotes({
    version: '1.5.0',
    base: 'v1.4.0',
    commits: [parseConventionalCommit('feat(x): stacked work (#1939) (#1940)')],
  });
  assert.match(draft, /- stacked work \(#1940\)/);
  assert.doesNotMatch(draft, /#1939/);
});

test('keeps a non-conventional subject instead of dropping it', () => {
  const parsed = parseConventionalCommit('Merge branch main');
  assert.equal(parsed.type, null);
  assert.equal(parsed.subject, 'Merge branch main');
  assert.equal(isInternal(parsed), false);
});

test('marks only known internal types as internal', () => {
  assert.equal(isInternal(parseConventionalCommit('refactor(x): move code')), true);
  assert.equal(isInternal(parseConventionalCommit('chore(ci): tidy')), true);
  assert.equal(isInternal(parseConventionalCommit('fix(x): repair')), false);
  assert.equal(isInternal(parseConventionalCommit('feat!: break')), false);
});

test('groups commits by category with user-visible work first', () => {
  const commits = [
    parseConventionalCommit('chore(ci): tidy'),
    parseConventionalCommit('fix(a): one (#1)'),
    parseConventionalCommit('feat(b): two (#2)'),
    parseConventionalCommit('random commit'),
  ];
  assert.deepEqual(groupCommits(commits).map((group) => group.title), [
    'Features',
    'Fixes',
    'Other',
    'Chores',
  ]);
});

test('renders a release-note draft with one H1 and no internal entries by default', () => {
  const commits = [
    parseConventionalCommit('feat(x): visible thing (#9)'),
    parseConventionalCommit('fix(y): visible fix (#10)'),
    parseConventionalCommit('refactor(z): internal move'),
  ];
  const draft = renderReleaseNotes({ version: '1.5.0', base: 'v1.4.0', commits });
  assert.ok(draft.startsWith('# Buildmesh v1.5.0'));
  assert.equal((draft.match(/^# /gm) ?? []).length, 1);
  assert.match(draft, /- visible thing \(#9\)/);
  assert.match(draft, /## Fixes/);
  assert.doesNotMatch(draft, /## Refactors/);
  assert.match(draft, /Internal commits excluded/);
  assert.match(draft, /refactor\(z\): internal move/);
});

test('surfaces breaking changes and includes internal work when asked', () => {
  const commits = [
    parseConventionalCommit('feat!: remove flag (#20)'),
    parseConventionalCommit('chore(deps): bump (#21)'),
  ];
  const draft = renderReleaseNotes({ version: '2.0.0', base: 'v1.9.0', commits, includeInternal: true });
  assert.match(draft, /## Breaking changes/);
  assert.match(draft, /- remove flag \(#20\)/);
  assert.match(draft, /## Chores/);
  assert.doesNotMatch(draft, /Internal commits excluded/);
});

test('promotes the real subject when the subject is a bare co-author trailer', () => {
  // Real squash merges in this repo have a subject of literally `@ (#1600)`;
  // the actual subject is the first body line.
  const parsed = parseConventionalCommit('@ (#1600)', 'feat(ui): finish accent calibration\n\nMore detail here.');
  assert.equal(parsed.type, 'feat');
  assert.equal(parsed.subject, 'finish accent calibration');
  assert.equal(parsed.pr, '1600');

  // A body of only trailers leaves nothing to promote, so the subject is kept
  // as-is; the entry is unhelpful but not silently replaced with a wrong one.
  const bare = parseConventionalCommit('@ (#1601)', '@someone <someone@example.com>');
  assert.equal(bare.subject, '@');
  assert.equal(bare.pr, '1601');
});

test('never turns a bare body #N into a PR link', () => {
  // Issue and cross-reference numbers are not PRs; a wrong published link is
  // worse than no number at all.
  const issue = parseConventionalCommit('feat(transcript): add Muse reader', 'Addresses issue #1708');
  assert.equal(issue.pr, null);
  const unrelated = parseConventionalCommit('fix(build-run): fifth-round review', 'See #1532 for context');
  assert.equal(unrelated.pr, null);
  // An explicit trailer is still honoured.
  const closes = parseConventionalCommit('fix(build-run): repair writer', 'Closes #1532');
  assert.equal(closes.pr, '1532');
  // ...unless the subject already cites it, which would render it twice.
  const repeated = parseConventionalCommit('feat(transcript): add Muse reader (issue #1708)', 'Closes #1708');
  assert.equal(repeated.pr, null);
  assert.equal(repeated.subject, 'add Muse reader (issue #1708)');
});

test('compares versions numerically, not lexically', () => {
  assert.ok(compareVersions('v1.10.0', 'v1.9.0') > 0);
  assert.ok(compareVersions('v1.3.0', 'v1.3.0') === 0);
  assert.ok(compareVersions('2.0.0', 'v1.99.99') > 0);
});

test('resolves the base to the release commit, not the first reachable tag', () => {
  // Reproduces this repo: only v1.0.0 is reachable from main, so a
  // reachability-only base yields 613 commits instead of the real range.
  const resolved = resolveBase({
    version: '1.4.0',
    tags: ['v1.0.0', 'v1.1.0', 'v1.2.0', 'v1.3.0'],
    releaseCommits: [
      { version: 'v1.3.0', commit: '8710bcf1' },
      { version: 'v1.0.0', commit: '7e3faecb' },
    ],
    reachableTag: 'v1.0.0',
  });
  assert.equal(resolved.ref, '8710bcf1');
  assert.match(resolved.source, /v1\.3\.0/);
});

test('base resolution ignores the release being prepared and future tags', () => {
  const resolved = resolveBase({
    version: '1.4.0',
    tags: ['v1.0.0', 'v1.3.0', 'v1.4.0', 'v1.5.0'],
    releaseCommits: [
      { version: 'v1.5.0', commit: 'aaaaaaa' },
      { version: 'v1.4.0', commit: 'bbbbbbb' },
      { version: 'v1.3.0', commit: 'ccccccc' },
    ],
    reachableTag: 'v1.0.0',
  });
  assert.equal(resolved.ref, 'ccccccc');
});

test('base resolution falls back when no release commit carries the tag', () => {
  const reachable = resolveBase({
    version: '1.4.0',
    tags: ['v1.0.0', 'v1.3.0'],
    releaseCommits: [],
    reachableTag: 'v1.0.0',
  });
  assert.equal(reachable.ref, 'v1.0.0');

  const highest = resolveBase({
    version: '1.4.0',
    tags: ['v1.0.0', 'v1.3.0'],
    releaseCommits: [],
    reachableTag: null,
  });
  assert.equal(highest.ref, 'v1.3.0');
});

test('the real repository resolves its own base to the v1.3.0 boundary', () => {
  // Guards the actual bug: if this ever regresses to reachability, the draft
  // silently includes hundreds of already-shipped commits.
  // fileURLToPath, not a manual leading-slash strip: that would turn
  // `/home/runner/...` into a relative path and `git` would fail with ENOENT.
  const repoRoot = fileURLToPath(new URL('../..', import.meta.url));
  const version = String(JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8')).version).replace(/-.*$/, '');
  const tags = execFileSync('git', ['tag', '--list', 'v*'], { cwd: repoRoot, encoding: 'utf8' }).split('\n').filter(Boolean);
  const log = execFileSync('git', ['log', '--first-parent', '--format=%s%x1f%H', 'HEAD'], { cwd: repoRoot, encoding: 'utf8' });
  const releaseCommits = log.split('\n').map((line) => {
    const [subject, hash] = line.split('\x1f');
    const match = String(subject ?? '').match(/^chore\(release\): v(\d+\.\d+\.\d+)/);
    return match && hash ? { version: `v${match[1]}`, commit: hash } : null;
  }).filter(Boolean);
  const resolved = resolveBase({ version, tags, releaseCommits, reachableTag: 'v1.0.0' });

  // Derive the expectation from the repo rather than pinning a hash, so a
  // rebase does not break the guard — what matters is the version, not the SHA.
  const expected = releaseCommits.find((entry) => entry.version === 'v1.3.0');
  assert.ok(expected, 'expected a chore(release): v1.3.0 commit on the first-parent history');
  assert.equal(resolved.ref, expected.commit, 'base must be the v1.3.0 release commit, not v1.0.0');
  assert.notEqual(resolved.ref, 'v1.0.0', 'must not fall back to the only reachable tag');
  const count = Number(execFileSync('git', ['rev-list', '--no-merges', '--count', `${resolved.ref}..HEAD`], { cwd: repoRoot, encoding: 'utf8' }).trim());
  const reachabilityCount = Number(execFileSync('git', ['rev-list', '--no-merges', '--count', 'v1.0.0..HEAD'], { cwd: repoRoot, encoding: 'utf8' }).trim());
  assert.ok(count < reachabilityCount, `range ${count} should be far smaller than reachability ${reachabilityCount}`);
  assert.ok(count > 0 && count < 200, `expected a release-sized range, got ${count}`);
});
