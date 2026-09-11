import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, copyFileSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'buildmesh-context-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, 'scripts'));
  copyFileSync(new URL('../../scripts/sync-ai-context.mjs', import.meta.url), join(root, 'scripts/sync-ai-context.mjs'));
  mkdirSync(join(root, '.claude/skills/example/assets'), { recursive: true });
  writeFileSync(join(root, 'CLAUDE.md'), '# Instructions\r\n');
  writeFileSync(join(root, '.claude/skills/example/SKILL.md'), '---\nname: example\n---\nInstructions');
  writeFileSync(join(root, '.claude/skills/example/assets/data.bin'), Buffer.from([0, 255, 1]));
  const run = (...args) => spawnSync(process.execPath, [join(root, 'scripts/sync-ai-context.mjs'), ...args], { encoding: 'utf8' });
  return { root, run };
}

test('refresh copies instructions and nested assets; check detects drift without writing', (t) => {
  const { root, run } = fixture(t);
  assert.equal(run('--check').status, 1);
  assert.equal(run().status, 0);
  assert.equal(readFileSync(join(root, 'AGENTS.md'), 'utf8'), '# Instructions\r\n');
  assert.deepEqual(readFileSync(join(root, '.agents/skills/example/assets/data.bin')), Buffer.from([0, 255, 1]));
  assert.equal(run('--check').status, 0);
  writeFileSync(join(root, 'CLAUDE.md'), '# Updated');
  assert.equal(run('--check').status, 1);
  assert.equal(readFileSync(join(root, 'AGENTS.md'), 'utf8'), '# Instructions\r\n');
  assert.equal(run().status, 0);
  assert.equal(run('--check').status, 0);
});

test('refresh replaces Windows skill link placeholders', (t) => {
  const { root, run } = fixture(t);
  mkdirSync(join(root, '.agents'));
  writeFileSync(join(root, '.agents/skills'), '../.claude/skills');
  assert.equal(run('--check').status, 1);
  assert.equal(run().status, 0);
  assert.equal(run('--check').status, 0);
});

test('refresh refuses unexpected files and obsolete skills', (t) => {
  const { root, run } = fixture(t);
  mkdirSync(join(root, '.agents'));
  writeFileSync(join(root, '.agents/skills'), 'user content');
  assert.equal(run().status, 1);
  assert.equal(readFileSync(join(root, '.agents/skills'), 'utf8'), 'user content');
  rmSync(join(root, '.agents/skills'));
  assert.equal(run().status, 0);
  writeFileSync(join(root, '.agents/skills/obsolete.md'), 'keep until explicitly removed');
  assert.equal(run('--check').status, 1);
  assert.equal(run().status, 1);
});
