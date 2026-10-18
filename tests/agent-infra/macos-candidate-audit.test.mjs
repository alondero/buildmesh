import { test } from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

const auditPath = fileURLToPath(new URL('../../scripts/ci/audit-macos-candidate.mjs', import.meta.url));
const mockPath = fileURLToPath(new URL('../fixtures/macos-audit-command.mjs', import.meta.url));

test('candidate audit CLI downloads, verifies and uploads a successful native audit', { skip: process.platform !== 'darwin' }, () => {
  const root = mkdtempSync(path.join(os.tmpdir(), 'buildmesh-macos-audit-cli-test-'));
  const bin = path.join(root, 'bin');
  const uploads = path.join(root, 'uploads');
  mkdirSync(bin);
  mkdirSync(uploads);

  const arch = process.arch === 'arm64' ? 'aarch64' : 'x86_64';
  const version = '1.5.0';
  const commands = ['codesign', 'find', 'gh', 'hdiutil', 'lipo', 'shasum', 'sw_vers', 'uname'];
  for (const command of commands) {
    const wrapperPath = path.join(bin, command);
    writeFileSync(wrapperPath, `#!/usr/bin/env node\nimport(${JSON.stringify(pathToFileURL(mockPath).href)}).then(({ runMock }) => runMock(${JSON.stringify(command)}, process.argv.slice(2))).catch((error) => { console.error(error); process.exitCode = 1; });\n`);
    chmodSync(wrapperPath, 0o755);
  }

  try {
    const result = spawnSync(process.execPath, [
      auditPath,
      '--tag', `v${version}`,
      '--commit', 'a'.repeat(40),
      '--arch', arch,
      '--minimum-macos', arch === 'aarch64' ? '11.0' : '10.13',
      '--repo', 'alondero/buildmesh',
    ], {
      encoding: 'utf8',
      env: {
        ...process.env,
        PATH: `${bin}${path.delimiter}${process.env.PATH}`,
        TMPDIR: root,
        AUDIT_TEST_ARCH: arch,
        AUDIT_TEST_VERSION: version,
        AUDIT_TEST_UPLOAD_DIR: uploads,
      },
    });

    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
    assert.match(result.stdout, /Uploaded MACOS-CANDIDATE-AUDIT/);
    const audit = JSON.parse(readFileSync(path.join(uploads, `MACOS-CANDIDATE-AUDIT-${arch}.json`), 'utf8'));
    assert.equal(audit.candidate.tag, `v${version}`);
    assert.equal(audit.candidate.commit, 'a'.repeat(40));
    assert.equal(audit.candidate.repository, 'alondero/buildmesh');
    assert.equal(audit.artifact.file, `Buildmesh_${version}_${arch === 'x86_64' ? 'x64' : arch}.dmg`);
    assert.match(audit.artifact.sha256, /^[a-f\d]{64}$/);
    assert.equal(audit.automatedAudit.outcome, 'PASS');
    assert.equal(audit.automatedAudit.runner.architecture, arch === 'aarch64' ? 'arm64' : 'x86_64');
    assert.equal(audit.automatedAudit.bundle.identifier, 'com.alond.buildmesh');
    assert.equal(audit.automatedAudit.bundle.version, version);
    assert.equal(audit.automatedAudit.bundle.signature, 'adhoc');
    assert.equal(audit.automatedAudit.bundle.minimumSystemVersion, arch === 'aarch64' ? '11.0' : '10.13');
    assert.deepEqual(audit.automatedAudit.bundle.architectures, [arch === 'aarch64' ? 'arm64' : 'x86_64']);
    assert.match(audit.humanEvidence.browserDownloadAndQuarantine, /^MANUAL_REQUIRED:/);
    assert.match(audit.humanEvidence.gatekeeperApproval, /^MANUAL_REQUIRED:/);
    assert.match(audit.humanEvidence.finderLaunch, /^MANUAL_REQUIRED:/);
    assert.deepEqual(readdirSync(root).sort(), ['bin', 'uploads']);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
