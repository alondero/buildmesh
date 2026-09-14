import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  classifyCommitCommand,
  decide,
  decideDocumentation,
  hasDocumentationExemption,
  isBehaviorSensitivePath,
  isDocumentationPath,
} from '../../.claude/hooks/guard-documentation.mjs';

const hookPath = fileURLToPath(new URL('../../.claude/hooks/guard-documentation.mjs', import.meta.url));

function decideFor(command, commitFiles) {
  return decideDocumentation({ command, commitFiles });
}

test('documentation guard scopes command parsing to real commit and message segments', () => {
  assert.deepEqual(classifyCommitCommand('git status'), { isCommit: false, isPlainCommit: false });
  assert.equal(classifyCommitCommand('git add src/App.tsx && git commit -m change').hasAddBefore, true);
  assert.equal(classifyCommitCommand('git commit --amend').hasAmend, true);
  assert.equal(classifyCommitCommand('git commit -am change').isPlainCommit, false);
  assert.equal(hasDocumentationExemption('git commit -m "docs: none"'), false);
  assert.equal(hasDocumentationExemption('git commit -m "docs: none — generated file"'), true);
  assert.equal(hasDocumentationExemption('git commit -m "feat" && echo "docs: none — generated file"'), false);
  assert.equal(hasDocumentationExemption('git commit -m "feat\n\ndocs: none — generated file"'), true);
  assert.equal(hasDocumentationExemption('git commit -m "feat"\necho "docs: none — generated file"'), false);
  assert.equal(hasDocumentationExemption('git commit -am "docs: none — generated file"'), true);
  assert.equal(classifyCommitCommand('git add src\\App.tsx && git commit -m change').hasAddBefore, true);
  assert.equal(isDocumentationPath('.github/workflows/build.yml'), false);
  assert.equal(isDocumentationPath('CONTEXT.md'), true);
});

test('documentation guard denies a behavior-sensitive commit without a decision', () => {
  const verdict = decideFor('git commit -m "feat: change the spawn flow"', ['src/components/SpawnMenu.tsx']);
  assert.equal(verdict.permissionDecision, 'deny');
  assert.match(verdict.permissionDecisionReason, /docs: none/);
  assert.doesNotMatch(verdict.permissionDecisionReason, /SpawnMenu/);
});

test('documentation guard handles each supported commit snapshot', () => {
  assert.equal(decideFor(
    'git commit -m "feat: change the spawn flow"',
    ['src/components/SpawnMenu.tsx', 'docs/user-guide.md', 'CHANGELOG.md'],
  ), null);
  assert.equal(decideFor('git add docs/user-guide.md src/App.tsx && git commit -m "feat: change the app"', [
    'src/App.tsx',
    'docs/user-guide.md',
  ]), null);
  assert.equal(decideFor('git add src/App.tsx && git commit -m "feat: change the app"', ['src/App.tsx']).permissionDecision, 'deny');
  assert.equal(decideFor('git add . && git commit -m "feat: change the app"', ['src/App.tsx']).permissionDecision, 'deny');
  assert.equal(decideFor('git add . && git commit -m "docs: only"', ['docs/user-guide.md']), null);
  assert.equal(decideFor('git commit --amend --no-edit', ['src/App.tsx']).permissionDecision, 'deny');
  assert.equal(decideFor('git commit --amend --no-edit', ['src/App.tsx', 'CHANGELOG.md']), null);
  assert.equal(decideFor('git commit -a -m "fix: docs only"', ['docs/user-guide.md']), null);
  assert.equal(decideFor('git commit -a -m "fix: source"', ['src/App.tsx']).permissionDecision, 'deny');
  assert.equal(decideFor('git commit -a -m "fix: source"', ['src/App.tsx', 'docs/user-guide.md']), null);
  assert.equal(decideFor('git commit -a -m "fix: docs: none — generated"', ['src/App.tsx']), null);
  assert.equal(decideFor('git commit -m "chore: generated update; docs: none — generated binding"', ['src/App.tsx']), null);
  assert.equal(decideFor('git commit -m "feat: source" && echo "docs: none — fake"', ['src/App.tsx']).permissionDecision, 'deny');
});

test('documentation decision logic uses an injected state provider', () => {
  const calls = [];
  const verdict = decide('git commit -m "feat: change"', (classification) => {
    calls.push(classification.isPlainCommit);
    return { commitFiles: ['src/App.tsx'] };
  });
  assert.equal(verdict.permissionDecision, 'deny');
  assert.deepEqual(calls, [true]);
  assert.equal(decide('git commit -m "feat: change"', () => { throw new Error('git unavailable'); }), null);
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

function createGitFixture() {
  const fixtureRoot = mkdtempSync(join(tmpdir(), 'buildmesh-doc-hook-'));
  execFileSync('git', ['init', '-q'], { cwd: fixtureRoot });
  execFileSync('git', ['config', 'user.email', 'test@example.com'], { cwd: fixtureRoot });
  execFileSync('git', ['config', 'user.name', 'Documentation Test'], { cwd: fixtureRoot });
  return fixtureRoot;
}

function runHook(fixtureRoot, command) {
  return spawnSync(process.execPath, [hookPath], {
    input: JSON.stringify({ tool_name: 'Bash', tool_input: { command }, cwd: fixtureRoot }),
    encoding: 'utf8',
  });
}

function denied(output) {
  return JSON.parse(output).hookSpecificOutput.permissionDecision === 'deny';
}

test('the executable hook is read-only and evaluates real staged, add, -a, and amend snapshots', () => {
  const fixtureRoot = createGitFixture();
  try {
    mkdirSync(join(fixtureRoot, 'src'));
    writeFileSync(join(fixtureRoot, 'src', 'App.tsx'), 'export const App = () => null;\n');
    execFileSync('git', ['add', 'src/App.tsx'], { cwd: fixtureRoot });

    const stagedBefore = execFileSync('git', ['diff', '--staged', '--name-only'], { cwd: fixtureRoot, encoding: 'utf8' });
    const plain = runHook(fixtureRoot, 'git commit -m "feat: change the app"');
    assert.equal(plain.status, 0);
    assert.equal(denied(plain.stdout), true);
    assert.equal(execFileSync('git', ['diff', '--staged', '--name-only'], { cwd: fixtureRoot, encoding: 'utf8' }), stagedBefore);

    writeFileSync(join(fixtureRoot, 'README.md'), '# README\n');
    execFileSync('git', ['add', 'README.md'], { cwd: fixtureRoot });
    const allowed = runHook(fixtureRoot, 'git commit -m "feat: change the app"');
    assert.equal(allowed.status, 0);
    assert.equal(allowed.stdout, '');

    execFileSync('git', ['commit', '-qm', 'baseline'], { cwd: fixtureRoot });
    writeFileSync(join(fixtureRoot, 'src', 'App.tsx'), 'export const App = () => "changed";\n');
    const addChain = runHook(fixtureRoot, 'git add src/App.tsx && git commit -m "feat: change the app"');
    assert.equal(addChain.status, 0);
    assert.equal(denied(addChain.stdout), true);

    writeFileSync(join(fixtureRoot, 'README.md'), '# README changed\n');
    const addAll = runHook(fixtureRoot, 'git add . && git commit -m "feat: add docs"');
    assert.equal(addAll.status, 0);
    assert.equal(addAll.stdout, '');

    execFileSync('git', ['add', 'src/App.tsx', 'README.md'], { cwd: fixtureRoot });
    const stagedAfter = execFileSync('git', ['diff', '--staged', '--name-only'], { cwd: fixtureRoot, encoding: 'utf8' });
    const stagedChain = runHook(fixtureRoot, 'git commit -m "feat: staged change"');
    assert.equal(stagedChain.status, 0);
    assert.equal(stagedChain.stdout, '');
    assert.equal(execFileSync('git', ['diff', '--staged', '--name-only'], { cwd: fixtureRoot, encoding: 'utf8' }), stagedAfter);

  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('the executable hook evaluates the final tracked snapshot for git commit --amend', () => {
  const fixtureRoot = createGitFixture();
  try {
    mkdirSync(join(fixtureRoot, 'src'));
    writeFileSync(join(fixtureRoot, 'src', 'App.tsx'), 'export const App = () => null;\n');
    execFileSync('git', ['add', 'src/App.tsx'], { cwd: fixtureRoot });
    execFileSync('git', ['commit', '-qm', 'baseline source'], { cwd: fixtureRoot });
    writeFileSync(join(fixtureRoot, 'src', 'App.tsx'), 'export const App = () => "amended";\n');

    const amend = runHook(fixtureRoot, 'git commit --amend --no-edit');
    assert.equal(amend.status, 0);
    assert.equal(denied(amend.stdout), true);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('ordinary amend ignores unrelated unstaged behavior-sensitive files', () => {
  const fixtureRoot = createGitFixture();
  try {
    mkdirSync(join(fixtureRoot, 'tests'));
    writeFileSync(join(fixtureRoot, 'tests', 'fixture.txt'), 'baseline\n');
    execFileSync('git', ['add', 'tests/fixture.txt'], { cwd: fixtureRoot });
    execFileSync('git', ['commit', '-qm', 'baseline tests'], { cwd: fixtureRoot });
    mkdirSync(join(fixtureRoot, 'src'));
    writeFileSync(join(fixtureRoot, 'src', 'App.tsx'), 'export const App = () => null;\n');

    const amend = runHook(fixtureRoot, 'git commit --amend --no-edit');
    assert.equal(amend.status, 0);
    assert.equal(amend.stdout, '');
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});

test('the executable hook allows a tracked docs-only git commit -a', () => {
  const fixtureRoot = createGitFixture();
  try {
    writeFileSync(join(fixtureRoot, 'README.md'), '# README\n');
    execFileSync('git', ['add', 'README.md'], { cwd: fixtureRoot });
    execFileSync('git', ['commit', '-qm', 'baseline'], { cwd: fixtureRoot });
    writeFileSync(join(fixtureRoot, 'README.md'), '# README updated\n');

    const result = runHook(fixtureRoot, 'git commit -a -m "docs: update README"');
    assert.equal(result.status, 0);
    assert.equal(result.stdout, '');
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }
});
