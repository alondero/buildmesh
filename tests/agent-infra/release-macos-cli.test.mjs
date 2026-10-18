import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmodSync, existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

const scriptPath = fileURLToPath(new URL('../../scripts/ci/publish-macos-checksums.mjs', import.meta.url));
const mockPath = fileURLToPath(new URL('../fixtures/macos-checksum-command.mjs', import.meta.url));
const tag = 'v1.5.0';
const architecture = 'x86_64';
const dmgName = 'Buildmesh_1.5.0_x64.dmg';
const updaterName = 'Buildmesh_1.5.0_x64.app.tar.gz';

function createFakeGh() {
  const root = mkdtempSync(path.join(os.tmpdir(), 'buildmesh-macos-checksum-cli-test-'));
  const bin = path.join(root, 'bin');
  const uploads = path.join(root, 'uploads');
  const callsPath = path.join(root, 'gh-calls.jsonl');
  mkdirSync(bin);
  mkdirSync(uploads);
  const ghPath = path.join(bin, 'gh');
  writeFileSync(ghPath,
    `#!/usr/bin/env node\nimport(${JSON.stringify(pathToFileURL(mockPath).href)})` +
    '.then(({ runMock }) => runMock(process.argv.slice(2)))' +
    '.catch((error) => { console.error(error); process.exitCode = 1; });\n');
  chmodSync(ghPath, 0o755);

  return {
    root,
    uploads,
    callsPath,
    env(extra = {}) {
      return {
        ...process.env,
        PATH: `${bin}${path.delimiter}${process.env.PATH ?? ''}`,
        CHECKSUM_TEST_CALLS: callsPath,
        CHECKSUM_TEST_UPLOADS: uploads,
        CHECKSUM_TEST_TAG: tag,
        ...extra,
      };
    },
    dispose() {
      rmSync(root, { recursive: true, force: true });
    },
  };
}

function runPublisher({ assetsPath, downloadDir, manifestPath, env }) {
  return spawnSync(process.execPath, [
    scriptPath,
    tag,
    architecture,
    assetsPath,
    downloadDir,
    manifestPath,
  ], { encoding: 'utf8', env });
}

function digest(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

test('checksum publisher CLI downloads listed release assets and writes the manifest consumed by upload', {
  skip: process.platform === 'win32',
}, () => {
  const fakeGh = createFakeGh();
  try {
    const releaseAssets = [dmgName, updaterName, `${updaterName}.sig`, 'latest.json'];
    const assetsPath = path.join(fakeGh.root, 'release-assets.json');
    const downloadDir = path.join(fakeGh.root, 'downloaded');
    const manifestPath = path.join(fakeGh.root, 'SHA256SUMS-macos-x86_64.txt');
    writeFileSync(assetsPath, JSON.stringify({ assets: releaseAssets.map((name) => ({ name })) }));

    const result = runPublisher({ assetsPath, downloadDir, manifestPath, env: fakeGh.env() });
    const expectedManifest = [
      `${digest('published DMG bytes')}  ${dmgName}`,
      `${digest('published updater bytes')}  ${updaterName}`,
      '',
    ].join('\n');
    assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
    assert.equal(result.stdout, expectedManifest);
    assert.equal(readFileSync(manifestPath, 'utf8'), expectedManifest);

    const upload = spawnSync('gh', ['release', 'upload', tag, manifestPath, '--clobber'], {
      encoding: 'utf8',
      env: fakeGh.env(),
    });
    assert.equal(upload.status, 0, `${upload.stdout}\n${upload.stderr}`);
    assert.equal(readFileSync(path.join(fakeGh.uploads, path.basename(manifestPath)), 'utf8'), expectedManifest);
    assert.deepEqual(readFileSync(fakeGh.callsPath, 'utf8').trim().split('\n').map(JSON.parse), [
      ['release', 'download', tag, '--pattern', dmgName, '--dir', downloadDir],
      ['release', 'download', tag, '--pattern', updaterName, '--dir', downloadDir],
      ['release', 'upload', tag, manifestPath, '--clobber'],
    ]);
  } finally {
    fakeGh.dispose();
  }
});

test('checksum publisher CLI rejects a missing published updater archive before downloading or writing a manifest', {
  skip: process.platform === 'win32',
}, () => {
  const fakeGh = createFakeGh();
  try {
    const assetsPath = path.join(fakeGh.root, 'release-assets.json');
    const downloadDir = path.join(fakeGh.root, 'downloaded');
    const manifestPath = path.join(fakeGh.root, 'SHA256SUMS-macos-x86_64.txt');
    writeFileSync(assetsPath, JSON.stringify({ assets: [{ name: dmgName }, { name: 'Buildmesh.app.tar.gz' }] }));

    const result = runPublisher({ assetsPath, downloadDir, manifestPath, env: fakeGh.env() });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /does not list expected updater archive Buildmesh_1\.5\.0_x64\.app\.tar\.gz/);
    assert.equal(existsSync(fakeGh.callsPath), false, 'no gh download should run before release assets are validated');
    assert.equal(existsSync(manifestPath), false);
  } finally {
    fakeGh.dispose();
  }
});

test('checksum publisher CLI leaves no manifest when a listed release asset download fails', {
  skip: process.platform === 'win32',
}, () => {
  const fakeGh = createFakeGh();
  try {
    const assetsPath = path.join(fakeGh.root, 'release-assets.json');
    const downloadDir = path.join(fakeGh.root, 'downloaded');
    const manifestPath = path.join(fakeGh.root, 'SHA256SUMS-macos-x86_64.txt');
    writeFileSync(assetsPath, JSON.stringify({ assets: [{ name: dmgName }, { name: updaterName }] }));

    const result = runPublisher({
      assetsPath,
      downloadDir,
      manifestPath,
      env: fakeGh.env({ CHECKSUM_TEST_FAIL_ASSET: updaterName }),
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /mock download failed for Buildmesh_1\.5\.0_x64\.app\.tar\.gz/);
    assert.deepEqual(readFileSync(fakeGh.callsPath, 'utf8').trim().split('\n').map(JSON.parse), [
      ['release', 'download', tag, '--pattern', dmgName, '--dir', downloadDir],
      ['release', 'download', tag, '--pattern', updaterName, '--dir', downloadDir],
    ]);
    assert.equal(existsSync(manifestPath), false);
  } finally {
    fakeGh.dispose();
  }
});
