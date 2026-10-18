#!/usr/bin/env node
// Audit the exact macOS DMG uploaded to a draft GitHub release. This checks
// bytes that users can download; it does not simulate a browser quarantine,
// Gatekeeper approval, or an interactive Finder launch.

import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import process from 'node:process';
import { macOSDmgName } from './macos-artifacts.mjs';

function fail(message) {
  throw new Error(`macOS candidate audit: ${message}`);
}

function option(name) {
  const index = process.argv.indexOf(name);
  if (index < 0 || !process.argv[index + 1]) fail(`missing ${name}`);
  return process.argv[index + 1];
}

function run(command, args, options = {}) {
  try {
    return execFileSync(command, args, {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      ...options,
    }).trim();
  } catch (error) {
    const detail = [error.stdout, error.stderr].filter(Boolean).join('\n').trim();
    fail(`${command} ${args.join(' ')} failed${detail ? `:\n${detail}` : ''}`);
  }
}

function runOutput(command, args) {
  const result = spawnSync(command, args, { encoding: 'utf8' });
  if (result.error || result.status !== 0) {
    const detail = [result.stdout, result.stderr, result.error?.message].filter(Boolean).join('\n').trim();
    fail(`${command} ${args.join(' ')} failed${detail ? `:\n${detail}` : ''}`);
  }
  return `${result.stdout}\n${result.stderr}`;
}

function plistValue(plist, key) {
  return run('/usr/libexec/PlistBuddy', ['-c', `Print :${key}`, plist]);
}

function requiredFileHash(manifest, filename) {
  const matching = readFileSync(manifest, 'utf8')
    .split(/\r?\n/)
    .map((line) => line.match(/^([a-f\d]{64})\s+\*?(.+)$/i))
    .filter((match) => match && match[2] === filename);
  if (matching.length !== 1) fail(`${path.basename(manifest)} must contain exactly one checksum for ${filename}`);
  return matching[0][1].toLowerCase();
}

function sha256(file) {
  return run('shasum', ['-a', '256', file]).split(/\s+/)[0].toLowerCase();
}

let scratch;
let mountDir;
let mounted = false;
let failure;

try {
  if (process.platform !== 'darwin') fail('must run on a native macOS runner');

  const tag = option('--tag');
  const commit = option('--commit');
  const architecture = option('--arch');
  const expectedMinimum = option('--minimum-macos');
  const repository = option('--repo');
  const version = tag.replace(/^v/, '');
  if (!/^v\d+\.\d+\.\d+$/.test(tag)) fail(`unexpected release tag ${tag}`);
  if (!/^[a-f\d]{40}$/i.test(commit)) fail('commit must be a full Git SHA');

  const expected = {
    aarch64: { cpu: 'arm64', minimum: '11.0' },
    x86_64: { cpu: 'x86_64', minimum: '10.13' },
  }[architecture];
  if (!expected) fail(`unsupported architecture ${architecture}`);
  if (expectedMinimum !== expected.minimum) {
    fail(`${architecture} must declare macOS ${expected.minimum}, got ${expectedMinimum}`);
  }

  const runnerArchitecture = run('uname', ['-m']);
  if (runnerArchitecture !== expected.cpu) {
    fail(`runner is ${runnerArchitecture}; ${architecture} requires native ${expected.cpu} evidence`);
  }

  const dmgName = macOSDmgName(version, architecture);
  const manifestName = `SHA256SUMS-macos-${architecture}.txt`;
  scratch = path.join(os.tmpdir(), `buildmesh-macos-candidate-${process.pid}`);
  const downloadDir = path.join(scratch, 'download');
  mountDir = path.join(scratch, 'mounted');
  mkdirSync(downloadDir, { recursive: true });
  mkdirSync(mountDir, { recursive: true });

  run('gh', [
    'release', 'download', tag,
    '--repo', repository,
    '--pattern', dmgName,
    '--pattern', manifestName,
    '--dir', downloadDir,
  ]);

  const dmgPath = path.join(downloadDir, dmgName);
  const manifestPath = path.join(downloadDir, manifestName);
  const manifestHash = requiredFileHash(manifestPath, dmgName);
  const actualHash = sha256(dmgPath);
  if (actualHash !== manifestHash) fail(`${dmgName} does not match ${manifestName}`);

  run('hdiutil', ['attach', '-readonly', '-nobrowse', '-noautoopen', '-mountpoint', mountDir, dmgPath]);
  mounted = true;

  const appPath = run('find', [mountDir, '-maxdepth', '3', '-type', 'd', '-name', 'Buildmesh.app', '-print', '-quit']);
  if (!appPath) fail(`${dmgName} contains no Buildmesh.app`);

  run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', appPath]);
  const signatureText = runOutput('codesign', ['-dv', '--verbose=4', appPath]);
  if (!/Signature=adhoc(?:\s|$)/m.test(signatureText)) {
    fail(`${dmgName} is not ad-hoc signed`);
  }

  const infoPlist = path.join(appPath, 'Contents', 'Info.plist');
  const bundleId = plistValue(infoPlist, 'CFBundleIdentifier');
  const bundleVersion = plistValue(infoPlist, 'CFBundleShortVersionString');
  const executableName = plistValue(infoPlist, 'CFBundleExecutable');
  const minimumSystemVersion = plistValue(infoPlist, 'LSMinimumSystemVersion');
  const binary = path.join(appPath, 'Contents', 'MacOS', executableName);
  const binaryArchitectures = run('lipo', ['-archs', binary]).split(/\s+/).filter(Boolean);

  if (bundleId !== 'com.alond.buildmesh') fail(`unexpected bundle identifier ${bundleId}`);
  if (bundleVersion !== version) fail(`bundle version ${bundleVersion} does not match tag ${version}`);
  if (minimumSystemVersion !== expected.minimum) {
    fail(`bundle declares macOS ${minimumSystemVersion}, expected ${expected.minimum}`);
  }
  if (!binaryArchitectures.includes(expected.cpu)) {
    fail(`bundle executable lacks ${expected.cpu}: ${binaryArchitectures.join(', ')}`);
  }

  const audit = {
    schemaVersion: 1,
    candidate: { tag, commit: commit.toLowerCase(), repository },
    artifact: { file: dmgName, sha256: actualHash, checksumManifest: manifestName },
    automatedAudit: {
      outcome: 'PASS',
      method: 'GitHub draft asset downloaded with gh; checksum matched; DMG mounted read-only; bundle inspected on native runner',
      runner: { macOSVersion: run('sw_vers', ['-productVersion']), architecture: runnerArchitecture },
      bundle: {
        identifier: bundleId,
        version: bundleVersion,
        architectures: binaryArchitectures,
        signature: 'adhoc',
        minimumSystemVersion,
      },
    },
    humanEvidence: {
      browserDownloadAndQuarantine: 'MANUAL_REQUIRED: gh download does not establish browser quarantine behavior',
      gatekeeperApproval: 'MANUAL_REQUIRED: use a clean Mac user profile and the documented approval path',
      finderLaunch: 'MANUAL_REQUIRED: this runner audit does not establish an interactive Finder launch',
    },
  };

  const output = path.join(scratch, `MACOS-CANDIDATE-AUDIT-${architecture}.json`);
  writeFileSync(output, `${JSON.stringify(audit, null, 2)}\n`, 'utf8');
  run('gh', ['release', 'upload', tag, output, '--repo', repository, '--clobber']);
  console.log(`Uploaded MACOS-CANDIDATE-AUDIT-${architecture}.json for ${tag} (${actualHash})`);
} catch (error) {
  failure = error;
} finally {
  if (mounted) {
    try {
      run('hdiutil', ['detach', mountDir, '-quiet']);
    } catch (error) {
      failure = failure
        ? new Error(`${failure.message}\nCleanup also failed: ${error.message}`)
        : error;
    }
  }
  if (scratch) {
    try {
      rmSync(scratch, { recursive: true, force: true });
    } catch (error) {
      failure = failure
        ? new Error(`${failure.message}\nScratch cleanup also failed: ${error.message}`)
        : error;
    }
  }
}

if (failure) {
  console.error(failure instanceof Error ? failure.message : String(failure));
  process.exitCode = 1;
}
