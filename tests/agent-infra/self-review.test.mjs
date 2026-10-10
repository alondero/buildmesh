import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { addedLines, checklistDigest, findClaims, findFormatChurn, selfReview } from '../../scripts/self-review.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const cli = join(root, 'scripts/self-review.mjs');

function repo(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'buildmesh-self-review-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const git = (...args) => execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  git('init', '-q');
  git('config', 'user.name', 'Agent test');
  git('config', 'user.email', 'agent-test@example.invalid');
  git('config', 'core.autocrlf', 'false');
  git('config', 'core.hooksPath', join(cwd, 'no-hooks'));
  const put = (file, text) => {
    mkdirSync(dirname(join(cwd, file)), { recursive: true });
    writeFileSync(join(cwd, file), text);
  };
  const commit = message => {
    git('add', '.');
    git('-c', 'commit.gpgsign=false', 'commit', '-qm', message);
    return git('rev-parse', 'HEAD').trim();
  };
  put('README.md', 'baseline\n');
  const base = commit('baseline');
  return { cwd, git, put, commit, base };
}

// A statement rustfmt would wrap, and the wrapped form: the same tokens, different layout.
// The unchanged separator keeps every statement in a hunk of its own, as scattered rustfmt output is.
const squashed = i => `let value_${i} = compute(alpha_${i}, beta_${i}, gamma_${i});\n// keep ${i}\n`;
const wrapped = i => `let value_${i} = compute(\n    alpha_${i},\n    beta_${i},\n    gamma_${i},\n);\n// keep ${i}\n`;
const range = n => Array.from({ length: n }, (_, i) => i);

test('addedLines reports committed, uncommitted and untracked additions with line numbers and skips generated files', t => {
  const { cwd, put, commit, base } = repo(t);
  put('src/a.ts', 'const a = 1;\n// committed note\n');
  put('src/types/generated/Wire.ts', '// never edit by hand\n');
  commit('add a');
  put('src/a.ts', 'const a = 1;\n// committed note\n// uncommitted note\n');
  put('src/new.ts', '// untracked note\n');
  const lines = addedLines(cwd, base);
  assert.deepEqual(
    lines.map(l => `${l.file}:${l.line}:${l.text}`).sort(),
    ['src/a.ts:1:const a = 1;', 'src/a.ts:2:// committed note', 'src/a.ts:3:// uncommitted note', 'src/new.ts:1:// untracked note'],
  );
});

test('findClaims flags absolute wording in comments, docs and test titles and ignores code and tables', () => {
  const added = [
    { file: 'src/a.rs', line: 4, text: '    // The retry never touches a claimed worktree.' },
    { file: 'src/a.rs', line: 9, text: '    let never = always + every;' },
    { file: 'docs/x.md', line: 3, text: 'Other harnesses are unchanged by this fix.' },
    { file: 'docs/x.md', line: 5, text: '| Every | row | cell |' },
    { file: 'tests/a.test.ts', line: 7, text: "  it('keeps every row in insertion order', () => {" },
    { file: 'src/b.ts', line: 2, text: '// Reads the file once.' },
  ];
  const { shown, total } = findClaims(added, { limit: 10 });
  assert.equal(total, 3);
  assert.deepEqual(shown.map(c => `${c.file}:${c.line}`), ['docs/x.md:3', 'tests/a.test.ts:7', 'src/a.rs:4']);
});

test('findClaims puts documents first, caps the list and reports how many were left out', () => {
  const added = [
    ...range(5).map(i => ({ file: 'src/c.rs', line: i + 1, text: `// claim ${i} is always true` })),
    { file: 'docs/y.md', line: 1, text: 'This is guaranteed by construction.' },
    { file: 'docs/y.md', line: 2, text: 'By construction nothing else writes it.' },
  ];
  const { shown, total } = findClaims(added, { limit: 3 });
  assert.equal(total, 7);
  assert.equal(shown.length, 3);
  assert.deepEqual(shown.slice(0, 2).map(c => c.file), ['docs/y.md', 'docs/y.md']);
});

test('findClaims spreads a capped list across files instead of exhausting the first one', () => {
  const added = [
    ...range(6).map(i => ({ file: 'docs/a.md', line: i + 1, text: `Claim ${i} is always true.` })),
    { file: 'src/z.rs', line: 3, text: '// This is never reached twice.' },
  ];
  const { shown, total } = findClaims(added, { limit: 4 });
  assert.equal(total, 7);
  assert.deepEqual(shown.map(c => `${c.file}:${c.line}`), ['docs/a.md:1', 'docs/a.md:2', 'src/z.rs:3', 'docs/a.md:3']);
  assert.equal(findClaims(added, { limit: Infinity }).shown.length, 7);
});

test('findFormatChurn flags a behaviour commit that mostly reformats and leaves a style commit alone', t => {
  const { cwd, put, commit } = repo(t);
  put('src/lib.rs', range(12).map(squashed).join('') + 'fn behaviour() -> u8 { 1 }\n');
  const seeded = commit('chore: seed');
  put('src/lib.rs', range(12).map(wrapped).join('') + 'fn behaviour() -> u8 { 2 }\n');
  const feature = commit('feat(x): change behaviour');
  const flagged = findFormatChurn(cwd, seeded, { threshold: 10 });
  assert.equal(flagged.length, 1);
  assert.equal(flagged[0].sha, feature.slice(0, 8));
  assert.equal(flagged[0].formatOnly, 12);
  assert.equal(flagged[0].hunks, 13);
  assert.match(flagged[0].files[0], /^src\/lib\.rs 12$/);

  // The same reformatting under a style subject is the remedy, not the problem.
  const second = repo(t);
  second.put('src/lib.rs', range(12).map(squashed).join(''));
  const secondSeed = second.commit('chore: seed');
  second.put('src/lib.rs', range(12).map(wrapped).join(''));
  second.commit('style(rust): format lib');
  assert.deepEqual(findFormatChurn(second.cwd, secondSeed, { threshold: 10 }), []);
});

test('findFormatChurn includes uncommitted work and ignores a few reformatted hunks', t => {
  const { cwd, put, commit } = repo(t);
  put('src/lib.rs', range(12).map(squashed).join(''));
  const seeded = commit('chore: seed');
  put('src/lib.rs', range(12).map(wrapped).join(''));
  const flagged = findFormatChurn(cwd, seeded, { threshold: 10 });
  assert.equal(flagged.length, 1);
  assert.equal(flagged[0].sha, 'uncommitted');
  assert.deepEqual(findFormatChurn(cwd, seeded, { threshold: 13 }), []);
});

test('real edits that only share some tokens are not formatting', t => {
  const { cwd, put, commit } = repo(t);
  put('src/lib.rs', range(12).map(squashed).join(''));
  const seeded = commit('chore: seed');
  put('src/lib.rs', range(12).map(i => `let value_${i} = compute(alpha_${i}, beta_${i}, delta_${i});\n// keep ${i}\n`).join(''));
  commit('feat: swap an argument');
  assert.deepEqual(findFormatChurn(cwd, seeded, { threshold: 1 }), []);
});

test('the checklist digest condenses the numbered items of docs/agents/self-review.md into short lines', () => {
  const digest = checklistDigest(root);
  assert.ok(digest.length >= 5, `expected at least five checklist items, got ${digest.length}`);
  for (const line of digest) assert.ok(line.length <= 170, `digest line too long (${line.length}): ${line}`);
  assert.match(digest[0], /^1\. Claims/);
});

test('selfReview prints the flagged sections only when something is flagged and always prints the digest', t => {
  const { cwd, put, commit, base } = repo(t);
  const clean = selfReview(cwd, base, { root });
  assert.match(clean, /Self-review prompts/);
  assert.doesNotMatch(clean, /Absolute claims/);
  assert.doesNotMatch(clean, /only reformat/);
  assert.match(clean, /1\. Claims/);

  put('docs/note.md', 'This path is only ever called from the drain, so nothing else can reach it.\n');
  commit('docs: note');
  const flagged = selfReview(cwd, base, { root });
  assert.match(flagged, /Absolute claims/);
  assert.match(flagged, /docs\/note\.md:1/);
});

test('the CLI never fails because of findings, and rejects a base that does not exist', t => {
  const { cwd, put, commit, base } = repo(t);
  put('docs/note.md', 'Nothing here is ever wrong.\n');
  commit('docs: note');
  const ok = spawnSync(process.execPath, [cli, '--base', base], { cwd, encoding: 'utf8' });
  assert.equal(ok.status, 0, ok.stderr);
  assert.match(ok.stdout, /Absolute claims/);
  const bad = spawnSync(process.execPath, [cli, '--base', 'missing-base'], { cwd, encoding: 'utf8' });
  assert.equal(bad.status, 1);
  assert.match(bad.stderr, /Self-review failed/);
});
