import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { categorisePath, classifyPaths } from '../../scripts/ci/changed-scope.mjs';

const script = fileURLToPath(new URL('../../scripts/ci/changed-scope.mjs', import.meta.url));

test('rust-only changes run the Rust graph and skip the frontend suites', () => {
  assert.deepEqual(classifyPaths(['src-tauri/src/lib.rs']), { rust: true, frontend: false });
  assert.deepEqual(classifyPaths(['src-tauri/Cargo.toml']), { rust: true, frontend: false });
});

test('frontend-only changes run the frontend suites and skip the Rust graph', () => {
  assert.deepEqual(classifyPaths(['src/App.tsx']), { rust: false, frontend: true });
  assert.deepEqual(classifyPaths(['tests/unit/store.test.ts']), { rust: false, frontend: true });
  assert.deepEqual(classifyPaths(['playwright.config.base.ts']), { rust: false, frontend: true });
});

test('generated bindings changes run both graphs so the drift gate runs', () => {
  // src/types/generated/ is produced by ts-rs during cargo test and is
  // drift-gated only by the rust-bindings job. Classifying it as frontend
  // alone would let a hand-edited binding merge while every Rust check
  // reports green-by-skip (review of PR #1991).
  assert.equal(categorisePath('src/types/generated/bindings.ts'), 'both');
  assert.deepEqual(classifyPaths(['src/types/generated/bindings.ts']), { rust: true, frontend: true });
  assert.deepEqual(classifyPaths(['src/types/generated/file.ts']), { rust: true, frontend: true });
});

test('docs-only changes trigger neither graph', () => {
  assert.deepEqual(classifyPaths(['docs/development/releasing.md']), { rust: false, frontend: false });
  assert.deepEqual(classifyPaths(['README.md']), { rust: false, frontend: false });
  assert.deepEqual(classifyPaths(['CLAUDE.md', '.claude/skills/verify/SKILL.md']), { rust: false, frontend: false });
});

test('CI, scripts, and unknown paths conservatively run both graphs', () => {
  assert.equal(categorisePath('.github/workflows/verify.yml'), 'both');
  assert.equal(categorisePath('scripts/ci/run-guarded.mjs'), 'both');
  assert.equal(categorisePath('some/unknown/file.bin'), 'both');
  assert.deepEqual(classifyPaths(['.github/workflows/verify.yml']), { rust: true, frontend: true });
});

test('a mixed pull request runs everything it touches', () => {
  assert.deepEqual(classifyPaths(['src-tauri/src/lib.rs', 'src/App.tsx']), { rust: true, frontend: true });
});

test('an empty change set runs neither graph', () => {
  assert.deepEqual(classifyPaths([]), { rust: false, frontend: false });
});

test('without a base revision the script reports the full scope', () => {
  const dir = mkdtempSync(join(tmpdir(), 'changed-scope-'));
  try {
    const output = join(dir, 'github-output.txt');
    const res = spawnSync(process.execPath, [script], {
      encoding: 'utf8',
      env: { ...process.env, GITHUB_OUTPUT: output },
    });
    assert.equal(res.status, 0, res.stderr);
    assert.equal(readFileSync(output, 'utf8'), 'rust=true\nfrontend=true\n');
    assert.match(res.stdout, /rust=true frontend=true/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('classifies a paths file into GitHub Actions outputs', () => {
  const dir = mkdtempSync(join(tmpdir(), 'changed-scope-'));
  try {
    const pathsFile = join(dir, 'paths.txt');
    const output = join(dir, 'github-output.txt');
    writeFileSync(pathsFile, 'src-tauri/src/lib.rs\nREADME.md\n');
    const res = spawnSync(process.execPath, [script, '--paths-file', pathsFile], {
      encoding: 'utf8',
      env: { ...process.env, GITHUB_OUTPUT: output },
    });
    assert.equal(res.status, 0, res.stderr);
    assert.equal(readFileSync(output, 'utf8'), 'rust=true\nfrontend=false\n');
    assert.match(res.stdout, /rust=true frontend=false/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
