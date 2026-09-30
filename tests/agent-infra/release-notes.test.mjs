import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  groupCommits,
  isInternal,
  parseConventionalCommit,
  renderReleaseNotes,
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
