import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  classifyCommitCommand,
  decideDocumentation,
  hasDocumentationExemption,
  isBehaviorSensitivePath,
  isDocumentationPath,
} from '../../.claude/hooks/guard-documentation.mjs';

const hookPath = fileURLToPath(new URL('../../.claude/hooks/guard-documentation.mjs', import.meta.url));

test('documentation guard ignores non-commit commands and inline staging', () => {
  assert.deepEqual(classifyCommitCommand('git status'), { isCommit: false, isPlainCommit: false });
  assert.equal(classifyCommitCommand('git add src/App.tsx && git commit -m change').hasAddBefore, true);
  assert.equal(classifyCommitCommand('git commit --amend').hasAmend, true);
  assert.equal(classifyCommitCommand('git commit -am change').isPlainCommit, false);
  assert.equal(hasDocumentationExemption('git commit -m "docs: none"'), false);
  assert.equal(hasDocumentationExemption('git commit -m "docs: none — generated file"'), true);
  assert.equal(isDocumentationPath('.github/workflows/build.yml'), false);
});

test('documentation guard denies a plain commit without a documentation decision', () => {
  const verdict = decideDocumentation({
    command: 'git commit -m "feat: change the spawn flow"',
    stagedFiles: ['src/components/SpawnMenu.tsx'],
  });
  assert.equal(verdict.permissionDecision, 'deny');
  assert.match(verdict.permissionDecisionReason, /docs: none/);
});

test('documentation guard allows documented and explicitly exempted changes', () => {
  assert.equal(decideDocumentation({
    command: 'git commit -m "feat: change the spawn flow"',
    stagedFiles: ['src/components/SpawnMenu.tsx', 'docs/user-guide.md', 'CHANGELOG.md'],
  }), null);
  assert.equal(decideDocumentation({
    command: 'git add docs/user-guide.md src/App.tsx && git commit -m "feat: change the app"',
    stagedFiles: [],
  }), null);
  assert.equal(decideDocumentation({
    command: 'git add src/App.tsx && git commit -m "feat: change the app"',
    stagedFiles: [],
  }).permissionDecision, 'deny');
  assert.equal(decideDocumentation({
    command: 'git add . && git commit -m "feat: change the app"',
    stagedFiles: [],
  }).permissionDecision, 'deny');
  assert.equal(decideDocumentation({
    command: 'git commit --amend',
    stagedFiles: ['src/App.tsx'],
  }).permissionDecision, 'deny');
  assert.equal(decideDocumentation({
    command: 'git commit -a -m "fix: docs: none — generated"',
    stagedFiles: [],
  }), null);
  assert.equal(decideDocumentation({
    command: 'git commit -m "chore: generated update; docs: none — generated binding"',
    stagedFiles: ['src/components/SpawnMenu.tsx'],
  }), null);
});

test('documentation path classification leaves tooling and generated files alone', () => {
  assert.equal(isDocumentationPath('docs/troubleshooting.md'), true);
  assert.equal(isDocumentationPath('README.md'), true);
  assert.equal(isDocumentationPath('src/App.tsx'), false);
  assert.equal(isBehaviorSensitivePath('src/types/generated/Provider.ts'), false);
  assert.equal(isBehaviorSensitivePath('tests/unit/app.test.tsx'), false);
  assert.equal(isBehaviorSensitivePath('scripts/check.ps1'), true);
  assert.equal(isBehaviorSensitivePath('.github/workflows/build.yml'), true);
  assert.equal(isBehaviorSensitivePath('src-tauri/Cargo.toml'), true);
  assert.equal(isBehaviorSensitivePath('src-tauri/src/http/routes.rs'), true);
});

test('the executable hook denies and then allows a real staged snapshot', () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-doc-hook-'));
  try {
    execFileSync('git', ['init', '-q'], { cwd: fixtureRoot });
    mkdirSync(join(fixtureRoot, 'src'));
    writeFileSync(join(fixtureRoot, 'src', 'App.tsx'), 'export const App = () => null;\n');
    execFileSync('git', ['add', 'src/App.tsx'], { cwd: fixtureRoot });

    const payload = JSON.stringify({
      tool_name: 'Bash',
      tool_input: { command: 'git commit -m "feat: change the app"' },
      cwd: fixtureRoot,
    });
    const denied = spawnSync(process.execPath, [hookPath], { input: payload, encoding: 'utf8' });
    assert.equal(denied.status, 0);
    assert.equal(JSON.parse(denied.stdout).hookSpecificOutput.permissionDecision, 'deny');

    mkdirSync(join(fixtureRoot, 'docs'));
    writeFileSync(join(fixtureRoot, 'docs', 'user-guide.md'), '# User guide\n');
    execFileSync('git', ['add', 'docs/user-guide.md'], { cwd: fixtureRoot });
    const allowed = spawnSync(process.execPath, [hookPath], { input: payload, encoding: 'utf8' });
    assert.equal(allowed.status, 0);
    assert.equal(allowed.stdout, '');
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});
