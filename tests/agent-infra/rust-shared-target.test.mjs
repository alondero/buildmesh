import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { artifactOwnerToken, assertOwnedLibraryBinary, compileOwnedTestBinaries, localPackageNames, packageName, snapshotExecutables } from '../../scripts/rust-test-shards.mjs';

test('package name is read from the package section', () => {
  assert.equal(packageName('[package]\nname = "buildmesh"\nversion = "1.0.0"\n\n[dependencies]\nserde = "1"\n'), 'buildmesh');
  assert.equal(packageName('[workspace]\nmembers = ["crate"]\n\n[package]\nname = "shared-shape"\n'), 'shared-shape');
  assert.equal(packageName('[dependencies]\nserde = "1"\n'), '');
});

test('local package names are the root crate and its path dependencies', () => {
  assert.deepEqual(localPackageNames('[package]\nname = "buildmesh"\n\n[dependencies]\nbuildmesh_macros = { path = "proc-macros" }\nserde = "1"\n'), ['buildmesh', 'buildmesh_macros']);
  const manifest = readFileSync(fileURLToPath(new URL('../../src-tauri/Cargo.toml', import.meta.url)), 'utf8');
  assert.deepEqual(localPackageNames(manifest), ['buildmesh', 'buildmesh_macros']);
});

test('a Buildmesh library binary without this worktree stamp is refused', () => {
  const token = artifactOwnerToken('F:/worktrees/alpha');
  const other = artifactOwnerToken('F:/worktrees/beta');
  assert.notEqual(token, other);
  assert.throws(() => assertOwnedLibraryBinary(Buffer.from(`preamble ${other} tail`), token), /another worktree/);
  assert.doesNotThrow(() => assertOwnedLibraryBinary(Buffer.from(`preamble ${token} tail`), token));
});

// Two crates, one Cargo target. Cargo's metadata hash ignores the manifest
// path, so the second compile is handed the first crate's executable and would
// export that crate's shape (issue #2339). The runner rebuilds the local
// package and each crate must execute its own binary.
test('a shared Cargo target cannot export another crate shape', { timeout: 180_000 }, async (t) => {
  const root = mkdtempSync(join(tmpdir(), 'buildmesh-shared-target-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const shared = join(root, 'target');
  const alphaDir = join(root, 'alpha');
  const betaDir = join(root, 'beta');
  writeCrate(alphaDir, 'alpha-field');
  writeCrate(betaDir, 'beta-field');

  const alpha = await compileOwnedTestBinaries(alphaDir, { targetDir: shared });
  const beta = await compileOwnedTestBinaries(betaDir, { targetDir: shared });
  assert.notEqual(alpha.lib, beta.lib);
  assert.notEqual(alpha.token, beta.token);

  assert.equal(exportedShape(alpha.lib, alphaDir, join(root, 'alpha-out')), 'alpha-field\n');
  assert.equal(exportedShape(beta.lib, betaDir, join(root, 'beta-out')), 'beta-field\n');
});

test('a snapshot keeps the compiled bytes after the shared file is replaced', (t) => {
  const root = mkdtempSync(join(tmpdir(), 'buildmesh-snapshot-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const sourceDir = join(root, 'deps');
  mkdirSync(sourceDir);
  const source = join(sourceDir, 'demo.exe');
  writeFileSync(source, 'alpha-bytes');
  writeFileSync(join(sourceDir, 'conpty.dll'), 'runtime');
  const [owned] = snapshotExecutables([{ kinds: ['rlib', 'cdylib'], name: 'demo', executable: source }], join(root, 'owned'));
  writeFileSync(source, 'beta-bytes');
  assert.equal(readFileSync(owned.executable, 'utf8'), 'alpha-bytes');
  assert.equal(readFileSync(join(root, 'owned', 'conpty.dll'), 'utf8'), 'runtime');
});

function writeCrate(dir, shape) {
  mkdirSync(join(dir, 'src'), { recursive: true });
  writeFileSync(join(dir, 'Cargo.toml'), '[package]\nname = "buildmesh"\nversion = "0.1.0"\nedition = "2021"\n');
  writeFileSync(join(dir, 'Cargo.lock'), 'version = 4\n\n[[package]]\nname = "buildmesh"\nversion = "0.1.0"\n');
  // Same contract as src-tauri/build.rs: forward the runner's stamp into the
  // test binary. Cargo's shared fingerprint otherwise treats this package as
  // fresh and hands back the other crate's executable.
  // Same FNV-1a as src-tauri/build.rs and artifactOwnerToken. The runner
  // refuses the binary when these drift.
  writeFileSync(join(dir, 'build.rs'), `fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_MANIFEST_DIR");
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in manifest.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    println!("cargo:rustc-env=BUILDMESH_ARTIFACT_OWNER=buildmesh-artifact-owner-{hash:016x}");
}
`);
  writeFileSync(join(dir, 'src/lib.rs'), `#[cfg(test)]
fn artifact_owner() -> &'static str {
    option_env!("BUILDMESH_ARTIFACT_OWNER").unwrap_or("")
}

#[test]
fn export_shape() {
    // A plain reference is not enough: the Windows linker drops it, and the
    // runner searches the executable for these bytes.
    std::hint::black_box(artifact_owner());
    let dir = std::env::var("EXPORT_DIR").unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(std::path::Path::new(&dir).join("Shape.txt"), "${shape}\\n").unwrap();
}
`);
}

function exportedShape(executable, cwd, exportDir) {
  mkdirSync(exportDir, { recursive: true });
  const result = spawnSync(executable, ['export_shape', '--exact', '--test-threads=1'], {
    cwd,
    env: { ...process.env, EXPORT_DIR: exportDir },
    encoding: 'utf8',
    timeout: 60_000,
  });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  return readFileSync(join(exportDir, 'Shape.txt'), 'utf8');
}
