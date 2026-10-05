#!/usr/bin/env node
// Gate the app version across every file that stores it.
//
// One version, five files: package.json, src-tauri/tauri.conf.json,
// src-tauri/Cargo.toml, the `buildmesh` entry in src-tauri/Cargo.lock, and
// package-lock.json. `scripts/set-version.mjs` is the fanout that keeps them
// in step, but a hand edit, a `git checkout` of one file, or a revert that
// takes the lockfile with it can leave them disagreeing — and a release commit
// once bumped four of the five, so nothing noticed until the lockfile was read
// again by npm.
//
// This is a pure read: it installs nothing, imports nothing, and needs no
// `npm ci`, so it can run as its own near-instant job on every pull request
// (verify.yml) rather than as one buried step in a 45-minute frontend gate.
//
// Usage:
//   node scripts/check-manifest-versions.mjs               # the five must agree
//   node scripts/check-manifest-versions.mjs --expect 1.3.0 # and equal 1.3.0
//
// `--expect` is what release.yml passes (the tag with its leading `v` already
// stripped), so a `vX.Y.Z` tag is checked against every file rather than the
// three top-level manifests.
//
// Exit codes: 0 the versions agree; 1 they disagree or a version cannot be read.

import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const argv = process.argv.slice(2);

const USAGE = 'Usage: node scripts/check-manifest-versions.mjs [--expect <version>]';

// Walk the arguments instead of filtering them: a filter that only looks at the
// `--` prefix passes every unknown flag straight through, which would turn
// this gate from "must equal the release tag" into "must agree with itself"
// after a mistyped flag name in release.yml.
let expected = null;
for (let i = 0; i < argv.length; i += 1) {
  const arg = argv[i];
  if (arg !== '--expect') fail(`Unknown argument \`${arg}\`. ${USAGE}`);
  const value = argv[i + 1];
  if (value === undefined || value.startsWith('--')) {
    fail(`\`--expect\` needs a version, e.g. --expect 1.3.0. ${USAGE}`);
  }
  expected = value;
  i += 1;
}

function fail(message) {
  console.error(`::error::${message}`);
  process.exit(1);
}

function read(file) {
  try {
    return readFileSync(path.join(root, file), 'utf8');
  } catch (error) {
    // A missing file is a failed check, never a pass: the version is unknown,
    // and an unknown version must not certify a release.
    fail(`Could not read ${file} (${error.code ?? error.message}).`);
  }
}

function jsonVersion(file) {
  let parsed;
  try {
    parsed = JSON.parse(read(file));
  } catch (error) {
    fail(`${file} is not valid JSON (${error.message}).`);
  }
  if (typeof parsed.version !== 'string') fail(`${file} has no top-level "version" string.`);
  return parsed.version;
}

// The crate's own `version = "..."`, bounded at the next section header the way
// applyTomlBlock bounds the writer. An unbounded scan past `[package]` would
// read a dependency's version out of a later block, so a state the writer
// refuses to produce could still read green here.
function cargoTomlVersion(file) {
  const text = read(file);
  const header = text.search(/^\[package\]/m);
  if (header === -1) fail(`Could not find a [package] block in ${file}.`);
  const bodyStart = header + text.slice(header).indexOf('\n') + 1;
  const nextHeader = text.slice(bodyStart).search(/^\[/m);
  const end = nextHeader === -1 ? text.length : bodyStart + nextHeader;
  const version = text.slice(bodyStart, end).match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) fail(`Could not find a version in the [package] block of ${file}.`);
  return version;
}

// Cargo.lock lists the crate like any other dependency, so the entry is found
// by name rather than by position.
function cargoLockVersion(file) {
  const match = read(file).match(
    /^\[\[package\]\]\nname = "buildmesh"\nversion = "([^"]+)"/m,
  );
  if (!match) fail(`Could not find a version for the buildmesh package in ${file}.`);
  return match[1];
}

// npm stores the root version twice: the top-level mirror of package.json, and
// the `packages[""]` entry. `npm install` rewrites both, so a version bump that
// only moved one of them comes straight back on the next install.
function packageLockVersions(file) {
  let parsed;
  try {
    parsed = JSON.parse(read(file));
  } catch (error) {
    fail(`${file} is not valid JSON (${error.message}).`);
  }
  const rootEntry = parsed.packages?.[''];
  if (!rootEntry || typeof rootEntry.version !== 'string') {
    fail(`${file} has no packages[""].version string.`);
  }
  if (typeof parsed.version !== 'string') fail(`${file} has no top-level "version" string.`);
  return { version: parsed.version, rootEntry: rootEntry.version };
}

const packageLock = packageLockVersions('package-lock.json');

const versions = [
  { file: 'package.json', version: jsonVersion('package.json') },
  { file: 'src-tauri/tauri.conf.json', version: jsonVersion('src-tauri/tauri.conf.json') },
  { file: 'src-tauri/Cargo.toml', version: cargoTomlVersion('src-tauri/Cargo.toml') },
  { file: 'src-tauri/Cargo.lock', version: cargoLockVersion('src-tauri/Cargo.lock') },
  { file: 'package-lock.json', version: packageLock.version },
  { file: 'package-lock.json (packages[""])', version: packageLock.rootEntry },
];

for (const { file, version } of versions) {
  console.log(`${file}: ${version}`);
}

// package.json is the reference because it is the one every other file
// mirrors: the Rust crates, the Tauri config, and both lockfile sites are
// copies of its version. The error names the version to converge on, not just
// the files that are behind.
const [reference, ...rest] = versions;
const offenders = rest.filter((entry) => entry.version !== reference.version);

if (expected !== null) {
  const wrong = versions.filter((entry) => entry.version !== expected);
  if (wrong.length > 0) {
    console.error(
      `::error::Expected ${expected} in every manifest, but ${wrong
        .map((entry) => `${entry.file} has ${entry.version}`)
        .join(', ')}. Bump every manifest before tagging.`,
    );
    process.exit(1);
  }
}

if (offenders.length > 0) {
  console.error(
    `::error::App version drift: ${offenders
      .map((entry) => `${entry.file} has ${entry.version}`)
      .join(', ')} but ${reference.file} has ${reference.version}. ` +
      `Run \`npm run version:set -- ${reference.version}\` and commit the result.`,
  );
  process.exit(1);
}

console.log(
  expected === null
    ? `all ${versions.length} version sites agree on ${reference.version}`
    : `all ${versions.length} version sites match the expected ${expected}`,
);
