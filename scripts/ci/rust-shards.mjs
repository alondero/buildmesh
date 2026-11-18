import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const workflowPath = path.join(repoRoot, '.github', 'workflows', 'verify.yml');

/** @returns {{ label: string, modules: string[] }[]} */
export function readShards(source = fs.readFileSync(workflowPath, 'utf8')) {
  const start = source.indexOf('  rust-tests:');
  if (start === -1) throw new Error('verify.yml has no `rust-tests` job. Sharding was removed; drop this gate or restore the job.');
  const matrixStart = source.indexOf('shard:', start);
  if (matrixStart === -1) throw new Error('verify.yml has no `shard:` matrix under `rust-tests`.');
  // The block ends at the next job-level key (`steps:`, `timeout-minutes:`),
  // which sits at a shallower indent than the matrix items.
  const rest = source.slice(matrixStart);
  const endMatch = rest.search(/\n {4}[a-z][\w-]*:/);
  const block = endMatch === -1 ? rest : rest.slice(0, endMatch + 1);

  const shards = [];
  // Deliberately strict about shape, tolerant about indentation width: each
  // entry must be a `- label: X` line followed by a `modules: "Y"` line, and
  // anything else in the block is a hard error rather than a silent skip.
  let pending = null;
  const labelOf = (line) => line.match(/^\s*- label: ([\w-]+)$/);
  const modulesOfLine = (line) => line.match(/^\s*modules: "(\*|[\p{ID_Start}_][\p{ID_Continue}]*(?: +[\p{ID_Start}_][\p{ID_Continue}]*)*)"$/u);
  for (const line of block.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed === 'shard:') continue;
    if (trimmed.startsWith('#')) continue;
    const label = labelOf(line);
    if (label) {
      if (pending) throw new Error(`Shard "${pending.label}" has no modules line:\n  ${line}`);
      pending = { label: label[1] };
      continue;
    }
    const modules = modulesOfLine(line);
    if (modules) {
      if (!pending) throw new Error(`A modules line has no preceding - label line:\n  ${line}`);
      shards.push({ label: pending.label, modules: modules[1].trim().split(/\s+/) });
      pending = null;
      continue;
    }
    if (/^\s*(fail-fast|matrix):/.test(line)) continue;
    throw new Error(
      `The \`rust-tests\` matrix has a line this gate does not understand:\n  ${line}\n` +
        'It parses that block directly, so keep each shard entry as exactly two lines: ' +
        '`- label: <name>` then `modules: "<top-level modules or *>"`.',
    );
  }
  if (pending) throw new Error(`Shard "${pending.label}" has no modules line.`);
  if (shards.length === 0) throw new Error('No shards found under the `rust-tests` matrix.');
  return shards;
}

// Partition the names emitted by the binary. Module ownership is exact,
// and the one wildcard shard owns every module the matrix does not name.
export function listedUnitTests(output) {
  return output.split(/\r?\n/).flatMap(line => line.endsWith(': test') ? [line.slice(0, -6).replace(/^test\s+/, '')] : []);
}

export function listCargoUnitTests(cwd) {
  return listedUnitTests(execFileSync('cargo', ['test', '--locked', '--lib', '--', '--list'], {
    cwd,
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'inherit'],
  }));
}

export function partitionUnitTests(shards, names) {
  if (!names.length) throw new Error('The Rust unit-test binary listed zero tests.');
  const byLabel = new Map();
  const owners = new Map();
  let remainder;
  for (const { label, modules } of shards) {
    if (byLabel.has(label)) throw new Error('Duplicate Rust shard: ' + label);
    byLabel.set(label, []);
    for (const module of modules) {
      if (module === '*') {
        if (remainder || modules.length !== 1) throw new Error('The wildcard must be the only module of one shard.');
        remainder = label;
      } else {
        if (owners.has(module)) throw new Error('Multiple shards own the Rust module ' + module);
        owners.set(module, label);
      }
    }
  }
  if (!remainder) throw new Error('The Rust shard matrix needs a wildcard owner.');
  const seen = new Set();
  for (const name of names) {
    if (!name || seen.has(name)) throw new Error(`Invalid or duplicate Rust test name: "${name}".`);
    seen.add(name);
    const end = name.indexOf('::');
    const owner = end < 0 ? undefined : owners.get(name.slice(0, end));
    byLabel.get(owner ?? remainder).push(name);
  }
  return byLabel;
}

// libtest accepts several ORed names with --exact. Budget below Windows'
// process command-line limit while letting Linux keep one process per shard.
export function exactTestBatches(names, limit = process.platform === 'win32' ? 20_000 : 250_000) {
  const batches = [];
  let batch = [];
  let size = 0;
  for (const name of names) {
    const bytes = Buffer.byteLength(name) + 4;
    if (batch.length && size + bytes > limit) {
      batches.push(batch);
      batch = [];
      size = 0;
    }
    batch.push(name);
    size += bytes;
  }
  if (batch.length) batches.push(batch);
  return batches;
}
