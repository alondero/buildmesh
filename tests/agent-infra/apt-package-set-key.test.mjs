// The apt .deb set is cached so the nine Linux Rust jobs do not each download
// the same 77.7 MB every pull request (issue #2046). The cache is only worth
// anything if a *hit* means the .debs are already on disk, and the key that
// decides what a hit means used to say nothing about the package set at all:
// it was the hash of the composite action's own file, so it changed only when
// somebody edited that file.
//
// That is not a theoretical gap. The `ubuntu-latest` image moved from Ubuntu
// 24.04 to 26.04 in October 2026, and the entry the old image left behind
// (`apt-debs-Linux-0468eee8...`, created 2026-10-12) was still keyed the same
// way. Every job restored it, reported "Cache hit", and then downloaded the
// whole 77.7 MB of the new suite anyway: 135 MB restored, 0 files reused,
// 77.7 MB fetched, in all nine jobs of run 39777829786 attempt 2 — apt's own
// "Need to get" and "Fetched" figures matched in every one of them. The
// action's own comment called that harmless -- "a stale entry costs a
// download and not a broken job" -- but the download *is* the setup cost: the
// `db` shard spent 12m20s of a 15-minute job inside it, and two shards were
// cancelled at the job cap before they ever reached a test.
//
// So the key has to name the package set it holds. These tests pin that
// contract on both halves: the script that computes the identity, and the
// composite action's use of it.
//
// Run with: node --test tests/agent-infra/apt-package-set-key.test.mjs

import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  packageSetDigest,
  parseUriFilenames,
  releaseCodename,
} from '../../scripts/ci/apt-package-set-key.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const action = fs.readFileSync(path.join(repoRoot, '.github', 'actions', 'install-linux-build-deps', 'action.yml'), 'utf8');

/** A line of `apt-get install --print-uris` output. */
function uriLine(name, size = 1024) {
  return `'http://azure.archive.ubuntu.com/ubuntu/pool/main/${name.slice(0, 1)}/${name}_amd64.deb' ${name}_amd64.deb ${size} SHA256:${'a'.repeat(64)}`;
}

// --------------------------------------------------------------- the script

test('the parser reads the .deb filenames out of --print-uris output', () => {
  const text = [
    "NOTE: This is only a simulation!",
    uriLine('zlib1_1.3.1-1'),
    'Get:1 http://example.invalid zlib1_1.3.1-1_amd64.deb 91338 SHA256:abc',
    uriLine('libssl3t64_3.0.13-0ubuntu3.4'),
    '',
  ].join('\n');
  assert.deepEqual(parseUriFilenames(text), ['zlib1_1.3.1-1_amd64.deb', 'libssl3t64_3.0.13-0ubuntu3.4_amd64.deb']);
});

test('the digest does not depend on the order apt happened to print the set in', () => {
  const a = uriLine('alpha_1.0-1');
  const b = uriLine('beta_2.0-1');
  assert.equal(packageSetDigest(`${a}\n${b}\n`), packageSetDigest(`${b}\n${a}\n`));
});

test('the digest changes when a package version changes', () => {
  // The whole point: a cache entry holding `zlib1_1.3.1-1_amd64.deb` is not a
  // hit for an install that wants `zlib1_1.3.2-1_amd64.deb`, so the two must
  // not share a key.
  const before = packageSetDigest(`${uriLine('zlib1_1.3.1-1')}\n`);
  const after = packageSetDigest(`${uriLine('zlib1_1.3.2-1')}\n`);
  assert.notEqual(before, after);
});

test('the digest changes when the package set changes', () => {
  const before = packageSetDigest(`${uriLine('alpha_1.0-1')}\n`);
  const after = packageSetDigest(`${uriLine('alpha_1.0-1')}\n${uriLine('beta_2.0-1')}\n`);
  assert.notEqual(before, after);
});

test('a repeated filename is one file', () => {
  assert.equal(
    packageSetDigest(`${uriLine('alpha_1.0-1')}\n${uriLine('alpha_1.0-1')}\n`),
    packageSetDigest(`${uriLine('alpha_1.0-1')}\n`),
  );
});

test('the release codename is read from the OS release file', () => {
  const file = path.join(os.tmpdir(), `os-release-${process.pid}`);
  fs.writeFileSync(file, 'NAME="Ubuntu"\nVERSION_ID="26.04"\nVERSION_CODENAME=resolute\n');
  try {
    assert.equal(releaseCodename(file), 'resolute');
  } finally {
    fs.unlinkSync(file);
  }
});

test('the older UBUNTU_CODENAME field is read when it is the only one there', () => {
  // Some images carry only the older field, and a missing codename fails the
  // step rather than falling back to an unscoped key.
  const file = path.join(os.tmpdir(), `os-release-old-${process.pid}`);
  fs.writeFileSync(file, 'NAME="Ubuntu"\nVERSION_ID="22.04"\nUBUNTU_CODENAME=jammy\n');
  try {
    assert.equal(releaseCodename(file), 'jammy');
  } finally {
    fs.unlinkSync(file);
  }
});

test('a missing OS release file yields no codename rather than a wrong one', () => {
  // A wrong codename would scope the cache key to the wrong release, which is
  // the failure this whole change exists to prevent.
  assert.equal(releaseCodename(path.join(os.tmpdir(), 'no-such-os-release')), '');
});

// ------------------------------------------------------- the composite action

test('the .deb cache key names the package set, not the action file', () => {
  // The old key was `hashFiles(action.yml)`: a value that says nothing about
  // which packages are in the entry.
  assert.ok(
    !action.includes('hashFiles('),
    'the .deb cache key must not be derived from the composite action\'s own file; that cannot tell one package set from another',
  );
  assert.match(action, /steps\.[\w-]+\.outputs\.digest/, 'the cache key must include the resolved package-set digest');
});

test('the .deb cache key is scoped to the OS release', () => {
  // A runner-image migration moves the suite; an entry from the old release is
  // not a partial hit for the new one.
  assert.match(action, /steps\.[\w-]+\.outputs\.codename/, 'the cache key must include the OS release codename');
  assert.match(
    action,
    /restore-keys:[\s\S]*?steps\.[\w-]+\.outputs\.codename/,
    'the fallback restore must stay inside the same release',
  );
});

test('the cache is restored after the package set is resolved and before the install', () => {
  // The key cannot be known before `apt-get install --print-uris` has said
  // which .debs the install wants, so the restore has to come after it. A
  // restore placed before the resolution can only use a key that does not
  // identify the set, which is the bug.
  const resolve = action.indexOf('--print-uris');
  const restore = action.search(/actions\/cache\/restore@/);
  const install = action.search(/apt-get "\$\{APT_OPTS\[@\]\}" install -y/);
  assert.ok(resolve > -1, 'the action no longer resolves the package set');
  assert.ok(restore > -1, 'the action no longer restores the .deb cache');
  assert.ok(install > -1, 'the action no longer installs the packages');
  assert.ok(resolve < restore, 'the package set must be resolved before the cache is restored');
  assert.ok(restore < install, 'the cache must be restored before the packages are installed');
});

test('the cache is saved after the install, so it holds the .debs the install fetched', () => {
  const install = action.search(/apt-get "\$\{APT_OPTS\[@\]\}" install -y/);
  const save = action.search(/actions\/cache\/save@/);
  assert.ok(save > -1, 'the action no longer saves the .deb cache');
  assert.ok(save > install, 'the .deb cache must be saved after the install');
});

test('the cache is saved unless the exact set is already stored', () => {
  // The entry has to follow the package set. `cache-hit` is true only when the
  // restore matched the *exact* key, which means the entry already holds this
  // set; anything else — a release-scoped fallback, or no entry at all —
  // means this set is not stored yet, and the job has to write it. A
  // write-once rule would freeze the cache on whatever set it happened to
  // hold: after an Ubuntu security update every job would download the
  // difference and nothing would ever be stored again.
  const saveAt = action.indexOf('actions/cache/save@');
  assert.ok(saveAt > -1, 'the action no longer saves the .deb cache');
  // The step's own block starts at its `- name:` line, which is *before* the
  // `uses:` line the condition sits above.
  const stepStart = action.lastIndexOf('\n    - name:', saveAt);
  const save = action.slice(stepStart);
  const condition = save.match(/^\s*if:\s*(.+)$/m)?.[1];
  assert.ok(condition, 'the save step has no condition');
  assert.match(condition, /cache-hit\s*!=\s*'true'/, `the save step must write the entry unless the exact set is already stored, got: ${condition}`);
  assert.doesNotMatch(condition, /cache-matched-key/, '`cache-matched-key` is set for a fallback hit too, so it cannot stand in for "the exact set is stored"');
});

test('the package list is defined once and shared with the install', () => {
  // Two copies of the package list drift, and the resolved set then names
  // packages the install never asks for -- a cache key that never matches.
  const definitions = action.match(/APT_PACKAGES="/g) ?? [];
  assert.equal(definitions.length, 1, `the package list is defined ${definitions.length} times; define it once and share it`);
  assert.match(action, /echo "APT_PACKAGES=\$APT_PACKAGES" >> "\$GITHUB_ENV"/, 'the package list must reach the install step');
});
