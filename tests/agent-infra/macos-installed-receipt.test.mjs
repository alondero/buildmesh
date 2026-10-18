import { afterEach, beforeEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { validateReceipt } from '../../scripts/ci/verify-macos-installed-receipt.mjs';
import { macOSDmgName, macOSUpdaterArchiveName } from '../../scripts/ci/macos-artifacts.mjs';

const TAG = 'v1.5.0';
const COMMIT = 'a'.repeat(40);
const EXPECTED = {
  aarch64: { cpu: 'arm64', minimum: '11.0' },
  x86_64: { cpu: 'x86_64', minimum: '10.13' },
};
const RUNTIME_CHECKS = [
  'browserDownloadQuarantine',
  'gatekeeperApproval',
  'finderLaunch',
  'cliDetected',
  'cliAuthenticated',
  'taskAndDiff',
  'closeReopen',
  'sessionResume',
  'terminalNonResumable',
];
const UPDATE_CHECKS = [
  'isolatedFeed',
  'publicLatestUntouched',
  'sameBundleIdentifierAndUpdaterKey',
  'validUpdateRelaunchedWithState',
  'badSignatureRejectedWithoutReplacing',
];
const RECEIPT_CLI = fileURLToPath(new URL('../../scripts/ci/verify-macos-installed-receipt.mjs', import.meta.url));

let releaseDir;

function status(outcome = 'UNVERIFIED', evidence = 'No native test machine was available for this check.') {
  return { outcome, evidence };
}

function makeReceipt() {
  const architectures = {};
  for (const [architecture, expected] of Object.entries(EXPECTED)) {
    const file = macOSDmgName('1.5.0', architecture);
    const updaterArchive = macOSUpdaterArchiveName('1.5.0', architecture);
    const sha256 = architecture === 'aarch64' ? '1'.repeat(64) : '2'.repeat(64);
    const auditAsset = `MACOS-CANDIDATE-AUDIT-${architecture}.json`;
    const audit = {
      schemaVersion: 1,
      candidate: { tag: TAG, commit: COMMIT, repository: 'alondero/buildmesh' },
      artifact: { file, sha256, checksumManifest: `SHA256SUMS-macos-${architecture}.txt` },
      automatedAudit: {
        outcome: 'PASS',
        runner: { macOSVersion: '15.0', architecture: expected.cpu },
        bundle: {
          identifier: 'com.alond.buildmesh',
          version: '1.5.0',
          architectures: [expected.cpu],
          signature: 'adhoc',
          minimumSystemVersion: expected.minimum,
        },
      },
    };
    writeFileSync(path.join(releaseDir, auditAsset), `${JSON.stringify(audit)}\n`);
    writeFileSync(
      path.join(releaseDir, `SHA256SUMS-macos-${architecture}.txt`),
      `${sha256}  ${file}\n${'3'.repeat(64)}  ${updaterArchive}\n`,
    );
    architectures[architecture] = {
      artifact: { file, sha256, auditAsset },
      runtime: {
        hostMacOSVersion: 'UNVERIFIED: no native machine available',
        hostArchitecture: 'UNVERIFIED: no native machine available',
        downloadBrowser: 'UNVERIFIED: no browser download performed',
        downloadedArtifactSha256: 'UNVERIFIED: no browser download performed',
        cli: { name: 'UNVERIFIED: no CLI tested', version: 'UNVERIFIED: no CLI tested' },
        checks: Object.fromEntries(RUNTIME_CHECKS.map((name) => [name, status()])),
      },
      updater: {
        feedEndpoint: 'UNVERIFIED: no isolated test feed available',
        testBuildCommit: 'UNVERIFIED: updater smoke not run',
        fromVersion: 'UNVERIFIED',
        toVersion: 'UNVERIFIED',
        bundleIdentifier: 'UNVERIFIED: updater smoke not run',
        updaterKeyFingerprint: 'UNVERIFIED: updater smoke not run',
        checks: Object.fromEntries(UPDATE_CHECKS.map((name) => [name, status()])),
      },
    };
  }
  return {
    schemaVersion: 1,
    candidate: {
      tag: TAG,
      commit: COMMIT,
      releaseUrl: `https://github.com/alondero/buildmesh/releases/tag/${TAG}`,
    },
    architectures,
    supportClaims: { testedArchitectures: [], testedHarnesses: [], limitation: 'Native runtime evidence is still pending.' },
  };
}

function verify(receipt) {
  return validateReceipt(receipt, { tag: TAG, commit: COMMIT, releaseDir });
}

function verifyThroughCli(receipt) {
  const receiptFile = path.join(releaseDir, 'receipt.json');
  writeFileSync(receiptFile, `${JSON.stringify(receipt)}\n`);
  return spawnSync(process.execPath, [
    RECEIPT_CLI,
    receiptFile,
    '--tag', TAG,
    '--commit', COMMIT,
    '--release-dir', releaseDir,
  ], { encoding: 'utf8' });
}

function markArchitecturePassed(receipt, architecture) {
  const entry = receipt.architectures[architecture];
  const expected = EXPECTED[architecture];
  entry.runtime.hostMacOSVersion = '15.0';
  entry.runtime.hostArchitecture = expected.cpu;
  entry.runtime.downloadBrowser = 'Safari';
  entry.runtime.downloadedArtifactSha256 = entry.artifact.sha256;
  entry.runtime.cli = { name: 'Claude Code', version: '2.0.0' };
  for (const name of RUNTIME_CHECKS) {
    entry.runtime.checks[name] = status('PASS', `Observed ${name} on native ${expected.cpu} macOS.`);
  }
  entry.updater.feedEndpoint = 'https://private-test.example.invalid/buildmesh/{{target}}/{{arch}}/{{current_version}}';
  entry.updater.testBuildCommit = COMMIT;
  entry.updater.fromVersion = '1.5.0-smoke.1';
  entry.updater.toVersion = '1.5.0-smoke.2';
  entry.updater.bundleIdentifier = 'com.alond.buildmesh';
  entry.updater.updaterKeyFingerprint = 'minisign-public-key-sha256:0123456789abcdef';
  for (const name of UPDATE_CHECKS) {
    entry.updater.checks[name] = status('PASS', `Observed ${name} on native ${expected.cpu} macOS.`);
  }
}

beforeEach(() => {
  releaseDir = mkdtempSync(path.join(os.tmpdir(), 'buildmesh-macos-receipt-'));
});

afterEach(() => {
  rmSync(releaseDir, { recursive: true, force: true });
});

test('explicitly unverified native evidence is a valid limited-support receipt', () => {
  const receipt = makeReceipt();
  const result = verify(receipt);
  assert.deepEqual(result.verifiedArchitectures, []);
});

test('receipt CLI accepts a limited receipt and rejects a claim beyond its evidence', () => {
  const allowed = verifyThroughCli(makeReceipt());
  assert.equal(allowed.status, 0, allowed.stderr);
  assert.match(allowed.stdout, /NOT VERIFIED; keep support claims limited/);

  const deniedReceipt = makeReceipt();
  deniedReceipt.supportClaims.testedArchitectures = ['aarch64'];
  const denied = verifyThroughCli(deniedReceipt);
  assert.equal(denied.status, 1);
  assert.match(denied.stderr, /tested architecture claim aarch64 exceeds passing receipt evidence/);
});

test('candidate tag, commit, checksum manifest and automated audit are bound together', () => {
  const receipt = makeReceipt();
  receipt.architectures.aarch64.artifact.sha256 = '9'.repeat(64);
  assert.throws(() => verify(receipt), /digest does not match the published checksum manifest/);

  const other = makeReceipt();
  other.candidate.commit = 'b'.repeat(40);
  assert.throws(() => verify(other), /commit does not match/);
});

test('missing outcomes and failed observations cannot pass the release receipt check', () => {
  const missing = makeReceipt();
  delete missing.architectures.aarch64.runtime.checks.finderLaunch;
  assert.throws(() => verify(missing), /finderLaunch must be an object/);

  const failed = makeReceipt();
  failed.architectures.x86_64.runtime.checks.gatekeeperApproval = status('FAIL', 'Gatekeeper would not allow the app to open.');
  assert.throws(() => verify(failed), /gatekeeperApproval failed/);
});

test('PASS evidence must replace the template prompt with an observed result', () => {
  const receipt = makeReceipt();
  receipt.architectures.aarch64.runtime.checks.finderLaunch = status(
    'PASS',
    'Record whether the app launched from Finder.',
  );
  assert.throws(() => verify(receipt), /must describe an observed result/);

  markArchitecturePassed(receipt, 'aarch64');
  receipt.architectures.aarch64.runtime.checks.finderLaunch = status(
    'PASS',
    'Observed the app launch from Finder on a native arm64 Mac.',
  );
  assert.deepEqual(verify(receipt).verifiedArchitectures, ['aarch64']);
});

test('a tested-architecture claim needs every runtime and updater check to pass', () => {
  const receipt = makeReceipt();
  receipt.supportClaims.testedArchitectures = ['aarch64'];
  assert.throws(() => verify(receipt), /tested architecture claim aarch64 exceeds/);
});

test('a private isolated updater feed cannot be replaced by the public latest feed', () => {
  const receipt = makeReceipt();
  markArchitecturePassed(receipt, 'aarch64');
  receipt.architectures.aarch64.updater.feedEndpoint = 'https://github.com/alondero/buildmesh/releases/latest/download/latest.json';
  assert.throws(() => verify(receipt), /must be isolated HTTPS/);
});

test('native runtime and both updater outcomes support only the evidence-backed architecture and CLI', () => {
  const receipt = makeReceipt();
  markArchitecturePassed(receipt, 'aarch64');
  receipt.supportClaims.testedArchitectures = ['aarch64'];
  receipt.supportClaims.testedHarnesses = [{ architecture: 'aarch64', name: 'Claude Code' }];
  const result = verify(receipt);
  assert.deepEqual(result.verifiedArchitectures, ['aarch64']);
  assert.deepEqual(result.claimedHarnesses, [{ architecture: 'aarch64', name: 'Claude Code' }]);
});

test('a tested-harness claim requires the quarantined browser download to match the candidate digest', () => {
  const receipt = makeReceipt();
  markArchitecturePassed(receipt, 'aarch64');
  receipt.architectures.aarch64.runtime.downloadedArtifactSha256 = 'UNVERIFIED: downloaded bytes were not hashed';
  receipt.architectures.aarch64.runtime.checks.browserDownloadQuarantine = status('UNVERIFIED', 'Browser quarantine and downloaded candidate bytes were not verified.');
  receipt.architectures.aarch64.runtime.checks.gatekeeperApproval = status('UNVERIFIED', 'Gatekeeper was not checked for the downloaded candidate.');
  receipt.architectures.aarch64.runtime.checks.finderLaunch = status('UNVERIFIED', 'Finder launch was not checked for the downloaded candidate.');
  receipt.supportClaims.testedHarnesses = [{ architecture: 'aarch64', name: 'Claude Code' }];
  assert.throws(() => verify(receipt), /tested harness claim Claude Code on aarch64 exceeds/);
});

test('updater evidence requires distinct versions from the candidate commit', () => {
  const receipt = makeReceipt();
  markArchitecturePassed(receipt, 'x86_64');
  receipt.architectures.x86_64.updater.toVersion = receipt.architectures.x86_64.updater.fromVersion;
  assert.throws(() => verify(receipt), /versions must be distinct/);

  const wrongCommit = makeReceipt();
  markArchitecturePassed(wrongCommit, 'x86_64');
  wrongCommit.architectures.x86_64.updater.testBuildCommit = 'f'.repeat(40);
  assert.throws(() => verify(wrongCommit), /test builds must use the candidate commit/);
});
