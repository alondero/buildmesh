// Real blocking-command benchmark. Each case runs in a fresh test process;
// repo/file generation occurs here, outside the measured process.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { artifactOwnerToken, assertOwnedLibraryBinary, compileOwnedTestBinaries } from './rust-test-shards.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const scratch = path.join(root, '.tmp', `git-status-bench-${process.pid}`);
fs.mkdirSync(scratch, { recursive: true });
const reads = Number(process.env.BUILDMESH_STATUS_BENCH_READS ?? 5);
if (!Number.isInteger(reads) || reads < 1) throw new Error('Reads must be a positive integer');
const crate = path.join(root, 'src-tauri');
const { lib } = process.argv[3] ? { lib: path.resolve(process.argv[3]) } : await compileOwnedTestBinaries(crate, {
  snapshotDir: path.join(scratch, 'bin'), stream: true,
});
assertOwnedLibraryBinary(lib, artifactOwnerToken(crate));
const baseline = process.argv[4] ? path.resolve(process.argv[4]) : null;
if (baseline) assertOwnedLibraryBinary(baseline, artifactOwnerToken(crate));
console.log(`Benchmark executable: ${lib}`);

function git(dir, ...args) {
  const result = spawnSync('git', ['-C', dir, ...args], { encoding: 'utf8' });
  if (result.status !== 0) throw new Error(result.stderr);
}
function generated(file, mib) {
  // 64-byte numbered records: repeated generated text with a final edit.
  const block = Buffer.from(Array.from({ length: 16384 }, (_, i) => `${String(i).padStart(8, '0')} ${'x'.repeat(54)}\n`).join(''));
  const fd = fs.openSync(file, 'w');
  try { for (let i = 0; i < mib; i++) fs.writeSync(fd, block); }
  finally { fs.closeSync(fd); }
}
function repo(name) {
  const dir = path.join(scratch, name);
  fs.mkdirSync(dir);
  git(dir, 'init', '-q');
  git(dir, 'config', 'user.name', 'Benchmark');
  git(dir, 'config', 'user.email', 'bench@example.invalid');
  git(dir, 'config', 'core.autocrlf', 'false');
  fs.writeFileSync(path.join(dir, 'small.txt'), 'before\n');
  git(dir, 'add', '.');
  git(dir, 'commit', '-qm', 'baseline');
  return dir;
}
const results = [];
function measure(name, paths, summary = false) {
  const variants = baseline ? [['current', lib], ['baseline', baseline]] : [['current', lib]];
  if (results.length % 4 === 0) variants.reverse();
  for (const [variant, executable] of variants) {
    const result = spawnSync(executable, ['commands::git_tests::status_bench::recurring_status', '--exact', '--ignored', '--nocapture'], {
      cwd: path.join(root, 'src-tauri'), encoding: 'utf8', maxBuffer: 8 * 1024 * 1024,
      env: { ...process.env, BUILDMESH_STATUS_BENCH_PATHS: JSON.stringify(paths),
        BUILDMESH_STATUS_BENCH_READS: String(reads), BUILDMESH_STATUS_BENCH_SUMMARY: summary ? '1' : '0' },
    });
    if (result.status !== 0) throw new Error(`${result.stdout}\n${result.stderr}`);
    const record = result.stdout.match(/^STATUS_BENCH (.+)$/m);
    if (!record) throw new Error(`No benchmark result: ${result.stdout}`);
    const value = { name, variant, ...JSON.parse(record[1]) };
    results.push(value);
    console.log(JSON.stringify(value));
  }
}
const clean = repo('clean');
measure('clean', [clean]);
const small = repo('small');
fs.writeFileSync(path.join(small, 'small.txt'), 'after\nextra\n');
measure('small', [small]);
for (const mib of [1, 10, 100]) {
  const dir = repo(`generated-${mib}`);
  generated(path.join(dir, 'generated.txt'), mib);
  git(dir, 'add', '.');
  git(dir, 'commit', '-qm', 'generated');
  fs.appendFileSync(path.join(dir, 'generated.txt'), 'edited\n');
  measure(`generated-${mib}MiB`, [dir]);
  if (mib === 10) {
    const fd = fs.openSync(path.join(dir, 'generated.txt'), 'r+');
    try { fs.writeSync(fd, Buffer.from('X'), 0, 1, 0); }
    finally { fs.closeSync(fd); }
    measure('generated-10MiB-interior', [dir]);
  }
}
const untracked = repo('untracked');
generated(path.join(untracked, 'untracked.txt'), 100);
measure('untracked-100MiB', [untracked]);
const many = repo('many');
for (let i = 0; i < 2000; i++) fs.writeFileSync(path.join(many, `file-${i}.txt`), 'before\n');
git(many, 'add', '.');
git(many, 'commit', '-qm', 'many');
for (let i = 0; i < 2000; i++) fs.writeFileSync(path.join(many, `file-${i}.txt`), 'after\nextra\n');
measure('many-2000', [many]);
measure('many-status-summary-overlap', [many], true);
const manyGenerated = repo('many-generated');
for (let i = 0; i < 100; i++) generated(path.join(manyGenerated, `generated-${i}.txt`), 1);
git(manyGenerated, 'add', '.');
git(manyGenerated, 'commit', '-qm', 'many generated');
for (let i = 0; i < 100; i++) fs.appendFileSync(path.join(manyGenerated, `generated-${i}.txt`), 'edited\n');
measure('many-generated-100x1MiB', [manyGenerated]);
const worktrees = [];
for (let i = 0; i < 4; i++) {
  const dir = path.join(scratch, `worktree-${i}`);
  git(clean, 'worktree', 'add', '--detach', dir, 'HEAD');
  generated(path.join(dir, 'untracked.txt'), 10);
  worktrees.push(dir);
}
measure('four-worktrees-10MiB', worktrees);
measure('four-worktrees-status-summary-overlap', worktrees, true);
const output = process.argv[2] ?? path.join(scratch, 'results.json');
fs.writeFileSync(output, `${JSON.stringify({ reads, platform: process.platform, arch: process.arch, results }, null, 2)}\n`);
console.log(`Results: ${output}. Fixtures and owned executable: ${scratch}`);
