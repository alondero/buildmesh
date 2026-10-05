import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';

// The two CI gates that read scripts/check-manifest-versions.mjs both use its
// exit code and nothing else, so these tests drive the real script as a
// subprocess and assert that contract. A gate that always exits 0 is worse
// than no gate, and a check that only compares three of the five files is the
// failure that shipped: a release commit bumped four of the five and left
// package-lock.json behind.
//
// The real manifests are staged into a throwaway tree rather than mutated in
// place, so the tests run against the same artifacts the gate reads (including
// the real 270 KB lockfile) without touching the working tree. The scripts are
// staged too: both resolve the repository root from their own path, so running
// the real ones with a different cwd would just re-check the real tree.
const root = fileURLToPath(new URL('../../', import.meta.url));
const SCRIPTS = ['check-manifest-versions.mjs', 'set-version.mjs'];
const MANIFESTS = [
  ['package.json'],
  ['package-lock.json'],
  ['src-tauri', 'tauri.conf.json'],
  ['src-tauri', 'Cargo.toml'],
  ['src-tauri', 'Cargo.lock'],
];

function stage() {
  const dir = mkdtempSync(join(tmpdir(), 'buildmesh-manifest-check-'));
  mkdirSync(join(dir, 'src-tauri'), { recursive: true });
  mkdirSync(join(dir, 'scripts'), { recursive: true });
  for (const segments of MANIFESTS) {
    cpSync(join(root, ...segments), join(dir, ...segments));
  }
  for (const name of SCRIPTS) {
    cpSync(join(root, 'scripts', name), join(dir, 'scripts', name));
  }
  return dir;
}

function run(dir, args = []) {
  try {
    const stdout = execFileSync(
      process.execPath,
      [join(dir, 'scripts', 'check-manifest-versions.mjs'), ...args],
      {
        cwd: dir,
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'pipe'],
      },
    );
    return { status: 0, stdout, stderr: '' };
  } catch (error) {
    const err = error;
    return { status: err.status ?? 1, stdout: '', stderr: String(err.stderr ?? '') };
  }
}

function setVersion(dir, version) {
  execFileSync(
    process.execPath,
    [join(dir, 'scripts', 'set-version.mjs'), version],
    { cwd: dir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] },
  );
}

function setLockVersion(dir, version) {
  const file = join(dir, 'package-lock.json');
  const lock = JSON.parse(readFileSync(file, 'utf8'));
  lock.version = version;
  lock.packages[''].version = version;
  writeFileSync(file, `${JSON.stringify(lock, null, 2)}\n`);
}

test('exits 0 when every manifest agrees on one version', () => {
  const dir = stage();
  try {
    setVersion(dir, '9.9.9-0');
    const result = run(dir);
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /all 6 version sites agree on 9\.9\.9-0/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('fails and names package-lock.json when only the lockfile is stale', () => {
  const dir = stage();
  try {
    setVersion(dir, '9.9.9-0');
    setLockVersion(dir, '9.9.8');
    const result = run(dir);
    assert.equal(result.status, 1);
    // Both of the lockfile's version sites, not just the top-level mirror.
    assert.match(result.stderr, /package-lock\.json has 9\.9\.8/);
    assert.match(result.stderr, /\(packages\[""\]\) has 9\.9\.8/);
    assert.match(result.stderr, /npm run version:set -- 9\.9\.9-0/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('fails when a single lockfile version site drifts from the other', () => {
  const dir = stage();
  try {
    setVersion(dir, '9.9.9-0');
    // The shape an install would not immediately repair: the top-level mirror
    // moved, the root package entry did not.
    const file = join(dir, 'package-lock.json');
    const lock = JSON.parse(readFileSync(file, 'utf8'));
    lock.packages[''].version = '9.9.8';
    writeFileSync(file, `${JSON.stringify(lock, null, 2)}\n`);
    const result = run(dir);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /\(packages\[""\]\) has 9\.9\.8/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('--expect fails when the version is not the one a release tag names', () => {
  const dir = stage();
  try {
    setVersion(dir, '9.9.9-0');
    assert.equal(run(dir, ['--expect', '9.9.9-0']).status, 0);
    const result = run(dir, ['--expect', '1.2.3']);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Expected 1\.2\.3 in every manifest/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('fails closed when a manifest cannot be read', () => {
  const dir = stage();
  try {
    setVersion(dir, '9.9.9-0');
    rmSync(join(dir, 'src-tauri', 'Cargo.toml'));
    const result = run(dir);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Could not read src-tauri\/Cargo\.toml/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('rejects an unrecognised argument instead of checking nothing', () => {
  const dir = stage();
  try {
    setVersion(dir, '9.9.9-0');
    // An unknown flag is the dangerous case: this gate is the release tag
    // check, so a flag that is silently accepted degrades "must equal the tag"
    // into "must agree with itself" without anything turning red.
    for (const args of [
      ['--totally-unknown-flag'],
      ['--expct', '9.9.9-0'],
      ['--expct', '--nope'],
      ['1.2.3'],
      ['--expect'],
      ['--expect', '--nope'],
    ]) {
      const result = run(dir, args);
      assert.equal(result.status, 1, `expected ${args.join(' ')} to be rejected`);
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('reads the Cargo.toml version from [package] only', () => {
  const dir = stage();
  try {
    setVersion(dir, '9.9.9-0');
    // A [package] block with no version of its own, followed by a section that
    // has one. An unbounded scan would report the later version and pass a
    // file the writer refuses to produce.
    writeFileSync(
      join(dir, 'src-tauri', 'Cargo.toml'),
      '[package]\nname = "buildmesh"\nedition = "2021"\n\n[dependencies]\nversion = "7.7.7"\n',
    );
    const result = run(dir);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /Could not find a version in the \[package\] block/);
    // The version must not have leaked in from the later section either.
    assert.doesNotMatch(result.stderr, /7\.7\.7/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
