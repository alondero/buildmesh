#!/usr/bin/env node
// Check that a macOS installed-candidate receipt is bound to the exact draft
// release and that every tested-support claim is backed by passing evidence.

import { readFileSync } from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { macOSDmgName, macOSUpdaterArchiveName } from './macos-artifacts.mjs';

const RECEIPT_TEMPLATE = JSON.parse(readFileSync(new URL('../../docs/release-evidence/macos-installed-candidate.template.json', import.meta.url), 'utf8'));

const ARCHITECTURES = {
  aarch64: { cpu: 'arm64', minimumSystemVersion: '11.0' },
  x86_64: { cpu: 'x86_64', minimumSystemVersion: '10.13' },
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
const CLI_CHECKS = ['cliDetected', 'cliAuthenticated', 'taskAndDiff', 'sessionResume'];

function requireValue(value, label) {
  if (typeof value !== 'string' || value.trim() === '' || /<[^>]+>/.test(value)) {
    throw new Error(`${label} must be filled in`);
  }
  return value.trim();
}

function requireObject(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value;
}

function requireStatus(check, label, templateEvidence) {
  requireObject(check, label);
  if (!['PASS', 'FAIL', 'UNVERIFIED'].includes(check.outcome)) {
    throw new Error(`${label}.outcome must be PASS, FAIL or UNVERIFIED`);
  }
  const evidence = requireValue(check.evidence, `${label}.evidence`);
  if (check.outcome === 'UNVERIFIED' && evidence.length < 12) {
    throw new Error(`${label} needs a reason when it is UNVERIFIED`);
  }
  if (check.outcome === 'PASS' && (evidence === templateEvidence || /^record\s/i.test(evidence))) {
    throw new Error(`${label}.evidence must describe an observed result, not a receipt prompt`);
  }
  if (check.outcome === 'FAIL') {
    throw new Error(`${label} failed: ${evidence}`);
  }
  return check.outcome;
}

function requireObservedValue(value, label) {
  const result = requireValue(value, label);
  if (/^unverified\b/i.test(result)) throw new Error(`${label} must be observed, not UNVERIFIED`);
  return result;
}

function readManifestHash(releaseDir, architecture, filename) {
  const manifest = path.join(releaseDir, `SHA256SUMS-macos-${architecture}.txt`);
  const matches = readFileSync(manifest, 'utf8')
    .split(/\r?\n/)
    .map((line) => line.match(/^([a-f\d]{64})\s+\*?(.+)$/i))
    .filter((match) => match && match[2] === filename);
  if (matches.length !== 1) {
    throw new Error(`${path.basename(manifest)} must contain exactly one checksum for ${filename}`);
  }
  return matches[0][1].toLowerCase();
}

function readAndCheckAudit(releaseDir, architecture, expected) {
  const auditName = `MACOS-CANDIDATE-AUDIT-${architecture}.json`;
  const audit = JSON.parse(readFileSync(path.join(releaseDir, auditName), 'utf8'));
  const cpu = ARCHITECTURES[architecture].cpu;
  const minimumSystemVersion = ARCHITECTURES[architecture].minimumSystemVersion;

  if (audit.schemaVersion !== 1) throw new Error(`${auditName} has an unsupported schemaVersion`);
  if (audit.candidate?.tag !== expected.tag) throw new Error(`${auditName} is for another release tag`);
  if (audit.candidate?.commit?.toLowerCase() !== expected.commit) throw new Error(`${auditName} is for another commit`);
  if (audit.artifact?.file !== expected.file) throw new Error(`${auditName} names another DMG`);
  if (audit.artifact?.sha256?.toLowerCase() !== expected.sha256) throw new Error(`${auditName} has another DMG digest`);
  if (audit.automatedAudit?.outcome !== 'PASS') throw new Error(`${auditName} did not pass the native artifact audit`);
  if (audit.automatedAudit?.runner?.architecture !== cpu) throw new Error(`${auditName} was not created on native ${cpu}`);
  if (audit.automatedAudit?.bundle?.identifier !== 'com.alond.buildmesh') throw new Error(`${auditName} has an unexpected bundle identifier`);
  if (audit.automatedAudit?.bundle?.version !== expected.version) throw new Error(`${auditName} has another bundle version`);
  if (audit.automatedAudit?.bundle?.signature !== 'adhoc') throw new Error(`${auditName} does not confirm ad-hoc signing`);
  if (audit.automatedAudit?.bundle?.minimumSystemVersion !== minimumSystemVersion) {
    throw new Error(`${auditName} has an unexpected minimum macOS version`);
  }
  if (!Array.isArray(audit.automatedAudit?.bundle?.architectures)
      || !audit.automatedAudit.bundle.architectures.includes(cpu)) {
    throw new Error(`${auditName} does not confirm the ${cpu} executable`);
  }
  requireValue(audit.automatedAudit?.runner?.macOSVersion, `${auditName}.runner.macOSVersion`);
}

/** Validate a receipt and return the architectures that support a tested claim. */
export function validateReceipt(receipt, { tag, commit, releaseDir }) {
  requireObject(receipt, 'receipt');
  if (receipt.schemaVersion !== 1) throw new Error('receipt.schemaVersion must be 1');
  const candidate = requireObject(receipt.candidate, 'receipt.candidate');
  if (candidate.tag !== tag) throw new Error(`receipt tag ${candidate.tag} does not match ${tag}`);
  const normalizedCommit = commit.toLowerCase();
  if (candidate.commit?.toLowerCase() !== normalizedCommit) throw new Error('receipt commit does not match the tagged commit');
  if (!/^v\d+\.\d+\.\d+$/.test(tag)) throw new Error(`unsupported release tag ${tag}`);
  if (requireValue(candidate.releaseUrl, 'receipt.candidate.releaseUrl') !== `https://github.com/alondero/buildmesh/releases/tag/${tag}`) {
    throw new Error('receipt releaseUrl must link to the exact Buildmesh candidate tag');
  }

  const architectures = requireObject(receipt.architectures, 'receipt.architectures');
  const verifiedArchitectures = [];
  const verifiedHarnesses = new Set();
  const version = tag.slice(1);

  for (const architecture of Object.keys(ARCHITECTURES)) {
    const entry = requireObject(architectures[architecture], `receipt.architectures.${architecture}`);
    const artifact = requireObject(entry.artifact, `${architecture}.artifact`);
    const expectedFile = macOSDmgName(version, architecture);
    const expectedUpdaterArchive = macOSUpdaterArchiveName(version, architecture);
    if (artifact.file !== expectedFile) throw new Error(`${architecture} artifact must be ${expectedFile}`);
    const digest = requireValue(artifact.sha256, `${architecture}.artifact.sha256`).toLowerCase();
    if (!/^[a-f\d]{64}$/.test(digest)) throw new Error(`${architecture} artifact.sha256 must be a SHA-256 digest`);
    if (readManifestHash(releaseDir, architecture, expectedFile) !== digest) {
      throw new Error(`${architecture} receipt digest does not match the published checksum manifest`);
    }
    readManifestHash(releaseDir, architecture, expectedUpdaterArchive);
    if (artifact.auditAsset !== `MACOS-CANDIDATE-AUDIT-${architecture}.json`) {
      throw new Error(`${architecture} auditAsset has an unexpected name`);
    }
    readAndCheckAudit(releaseDir, architecture, { tag, commit: normalizedCommit, version, file: expectedFile, sha256: digest });

    const runtime = requireObject(entry.runtime, `${architecture}.runtime`);
    requireValue(runtime.hostMacOSVersion, `${architecture}.runtime.hostMacOSVersion`);
    requireValue(runtime.hostArchitecture, `${architecture}.runtime.hostArchitecture`);
    requireValue(runtime.downloadBrowser, `${architecture}.runtime.downloadBrowser`);
    const downloadedHash = requireValue(runtime.downloadedArtifactSha256, `${architecture}.runtime.downloadedArtifactSha256`).toLowerCase();
    if (runtime.checks?.browserDownloadQuarantine?.outcome === 'PASS' && downloadedHash !== digest) {
      throw new Error(`${architecture} browser-downloaded bytes do not match the candidate DMG digest`);
    }
    if (runtime.checks?.browserDownloadQuarantine?.outcome === 'PASS'
        && /^unverified\b/i.test(runtime.downloadBrowser)) {
      throw new Error(`${architecture} passed browser checks need the browser name`);
    }
    const cli = requireObject(runtime.cli, `${architecture}.runtime.cli`);
    requireValue(cli.name, `${architecture}.runtime.cli.name`);
    requireValue(cli.version, `${architecture}.runtime.cli.version`);
    const runtimeChecks = requireObject(runtime.checks, `${architecture}.runtime.checks`);
    for (const name of RUNTIME_CHECKS) {
      requireStatus(
        runtimeChecks[name],
        `${architecture}.runtime.checks.${name}`,
        RECEIPT_TEMPLATE.architectures[architecture].runtime.checks[name].evidence,
      );
    }
    if (RUNTIME_CHECKS.some((name) => runtimeChecks[name].outcome === 'PASS')) {
      if (runtime.hostArchitecture !== ARCHITECTURES[architecture].cpu) {
        throw new Error(`${architecture} runtime evidence must come from a native ${ARCHITECTURES[architecture].cpu} host`);
      }
      if (/^unverified\b/i.test(runtime.hostMacOSVersion)) {
        throw new Error(`${architecture} passed runtime checks need the tested macOS version`);
      }
    }

    const update = requireObject(entry.updater, `${architecture}.updater`);
    requireValue(update.feedEndpoint, `${architecture}.updater.feedEndpoint`);
    const updateChecks = requireObject(update.checks, `${architecture}.updater.checks`);
    for (const name of UPDATE_CHECKS) {
      requireStatus(
        updateChecks[name],
        `${architecture}.updater.checks.${name}`,
        RECEIPT_TEMPLATE.architectures[architecture].updater.checks[name].evidence,
      );
    }

    if (updateChecks.isolatedFeed.outcome === 'PASS') {
      if (!/^https:\/\//i.test(update.feedEndpoint) || /releases\/latest/i.test(update.feedEndpoint)) {
        throw new Error(`${architecture} updater feed must be isolated HTTPS and must not use the public latest feed`);
      }
    }
    if (updateChecks.sameBundleIdentifierAndUpdaterKey.outcome === 'PASS') {
      if (update.bundleIdentifier !== 'com.alond.buildmesh') {
        throw new Error(`${architecture} updater test must use com.alond.buildmesh`);
      }
      requireObservedValue(update.updaterKeyFingerprint, `${architecture}.updater.updaterKeyFingerprint`);
    }

    const validUpdatePassed = updateChecks.validUpdateRelaunchedWithState.outcome === 'PASS';
    const badSignaturePassed = updateChecks.badSignatureRejectedWithoutReplacing.outcome === 'PASS';
    if (validUpdatePassed || badSignaturePassed) {
      for (const name of ['isolatedFeed', 'publicLatestUntouched', 'sameBundleIdentifierAndUpdaterKey']) {
        if (updateChecks[name].outcome !== 'PASS') {
          throw new Error(`${architecture} updater result requires ${name} to pass`);
        }
      }
      if (update.fromVersion === update.toVersion) throw new Error(`${architecture} updater test versions must be distinct`);
      requireObservedValue(update.fromVersion, `${architecture}.updater.fromVersion`);
      requireObservedValue(update.toVersion, `${architecture}.updater.toVersion`);
      if (update.testBuildCommit?.toLowerCase() !== normalizedCommit) {
        throw new Error(`${architecture} updater test builds must use the candidate commit`);
      }
    }

    const allPassed = RUNTIME_CHECKS.every((name) => runtimeChecks[name].outcome === 'PASS')
      && UPDATE_CHECKS.every((name) => updateChecks[name].outcome === 'PASS');
    if (allPassed) verifiedArchitectures.push(architecture);

    const candidateInstallPassed = runtimeChecks.browserDownloadQuarantine.outcome === 'PASS'
      && runtimeChecks.gatekeeperApproval.outcome === 'PASS'
      && runtimeChecks.finderLaunch.outcome === 'PASS'
      && downloadedHash === digest;
    if (candidateInstallPassed && CLI_CHECKS.every((name) => runtimeChecks[name].outcome === 'PASS')) {
      if (/^unverified\b/i.test(cli.name) || /^unverified\b/i.test(cli.version)) {
        throw new Error(`${architecture} passing CLI checks need the CLI name and version`);
      }
      verifiedHarnesses.add(`${architecture}\u0000${cli.name}`);
    }
  }

  for (const name of Object.keys(architectures)) {
    if (!(name in ARCHITECTURES)) throw new Error(`unexpected architecture entry ${name}`);
  }

  const claims = requireObject(receipt.supportClaims, 'receipt.supportClaims');
  if (!Array.isArray(claims.testedArchitectures) || !Array.isArray(claims.testedHarnesses)) {
    throw new Error('supportClaims.testedArchitectures and testedHarnesses must be arrays');
  }
  requireValue(claims.limitation, 'supportClaims.limitation');
  for (const architecture of claims.testedArchitectures) {
    if (!verifiedArchitectures.includes(architecture)) {
      throw new Error(`tested architecture claim ${architecture} exceeds passing receipt evidence`);
    }
  }
  for (const harness of claims.testedHarnesses) {
    requireObject(harness, 'supportClaims.testedHarnesses entry');
    const name = requireValue(harness.name, 'supportClaims.testedHarnesses.name');
    const architecture = requireValue(harness.architecture, 'supportClaims.testedHarnesses.architecture');
    if (!verifiedHarnesses.has(`${architecture}\u0000${name}`)) {
      throw new Error(`tested harness claim ${name} on ${architecture} exceeds passing receipt evidence`);
    }
  }

  return { verifiedArchitectures, claimedHarnesses: claims.testedHarnesses };
}

function usage() {
  return 'Usage: node scripts/ci/verify-macos-installed-receipt.mjs <receipt.json> --tag vX.Y.Z --commit <full-sha> --release-dir <downloaded-assets-dir>';
}

function main() {
  const [receiptFile, ...args] = process.argv.slice(2);
  if (!receiptFile) throw new Error(usage());
  const valueFor = (name) => {
    const index = args.indexOf(name);
    return index >= 0 ? args[index + 1] : undefined;
  };
  const tag = valueFor('--tag');
  const commit = valueFor('--commit');
  const releaseDir = valueFor('--release-dir');
  if (!tag || !commit || !releaseDir) throw new Error(usage());
  if (!/^[a-f\d]{40}$/i.test(commit)) throw new Error('--commit must be a full Git SHA');

  const receipt = JSON.parse(readFileSync(receiptFile, 'utf8'));
  const result = validateReceipt(receipt, { tag, commit, releaseDir });
  console.log(`Receipt matches ${tag} at ${commit}.`);
  for (const architecture of Object.keys(ARCHITECTURES)) {
    const verified = result.verifiedArchitectures.includes(architecture);
    console.log(`${architecture}: ${verified ? 'native runtime and updater evidence passed' : 'NOT VERIFIED; keep support claims limited'}`);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  try {
    main();
  } catch (error) {
    console.error(`macOS receipt: ${error.message}`);
    process.exitCode = 1;
  }
}
