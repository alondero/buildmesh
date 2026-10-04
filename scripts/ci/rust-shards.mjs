// The Rust unit-test shards, read out of the `rust-tests` matrix in
// `.github/workflows/verify.yml`. CI runs one job per shard; the local runner
// (scripts/rust-test-shards.mjs) runs one process per shard; the coverage gate
// (scripts/check-rust-shard-coverage.mjs) proves the shards claim every unit
// test exactly once. The workflow stays the single source of the split.
//
// Dependency-free on purpose: no YAML parser is available in this repo, and
// adding one to read a block we generate ourselves is not worth it. The matrix
// format is fixed, so parse it strictly and throw if it ever changes shape.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const workflowPath = path.join(repoRoot, '.github', 'workflows', 'verify.yml');

/** @returns {{ label: string, args: string }[]} */
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
  // entry must be a `- label: X` line followed by an `args: "Y"` line, and
  // anything else in the block is a hard error rather than a silent skip.
  let pending = null;
  const labelOf = (line) => line.match(/^\s*- label: (\S+)$/);
  const argsOfLine = (line) => line.match(/^\s*args: "(.*)"$/);
  for (const line of block.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed === 'shard:') continue;
    if (trimmed.startsWith('#')) continue;
    const label = labelOf(line);
    if (label) {
      if (pending) throw new Error(`Shard "${pending.label}" has no args line:\n  ${line}`);
      pending = { label: label[1] };
      continue;
    }
    const args = argsOfLine(line);
    if (args) {
      if (!pending) throw new Error(`An args line has no preceding - label line:\n  ${line}`);
      shards.push({ label: pending.label, args: args[1] });
      pending = null;
      continue;
    }
    if (/^\s*(fail-fast|matrix):/.test(line)) continue;
    throw new Error(
      `The \`rust-tests\` matrix has a line this gate does not understand:\n  ${line}\n` +
        'It parses that block directly, so keep each shard entry as exactly two lines: ' +
        '`- label: <name>` then `args: "<libtest arguments>"`.',
    );
  }
  if (pending) throw new Error(`Shard "${pending.label}" has no args line.`);
  if (shards.length === 0) throw new Error('No shards found under the `rust-tests` matrix.');
  return shards;
}
