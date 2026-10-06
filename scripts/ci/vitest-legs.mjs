// The vitest legs, read out of the `quality-tests` matrix in
// `.github/workflows/verify.yml`. CI runs one job per leg. The workflow stays
// the single source of the split, and `tests/agent-infra/vitest-legs.test.mjs`
// proves the legs still partition the suite the single combined invocation used
// to name — the vitest-side twin of the Rust shard-coverage gate.
//
// Dependency-free on purpose, for the same reason as `rust-shards.mjs`: no YAML
// parser is available in this repo, and adding one to read a block we generate
// ourselves is not worth it. The matrix format is fixed, so parse it strictly
// and throw if it ever changes shape.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const workflowPath = path.join(repoRoot, '.github', 'workflows', 'verify.yml');

/**
 * The suite directories the single `vitest run tests/unit tests/integration`
 * invocation used to name. The legs below must cover exactly this set: a
 * directory added here without a leg silently stops running, and a leg naming
 * something else runs tests the combined invocation never did.
 */
export const COVERED_SUITES = ['tests/unit', 'tests/integration'];

/**
 * @returns {{ label: string, args: string, shard: { index: number, of: number } | null, suites: string[] }[]}
 */
export function readVitestLegs(source = fs.readFileSync(workflowPath, 'utf8')) {
  const start = source.indexOf('  quality-tests:');
  if (start === -1) {
    throw new Error('verify.yml has no `quality-tests` job. The vitest legs were merged back into one job; drop this gate or restore the matrix.');
  }
  const matrixStart = source.indexOf('leg:', start);
  if (matrixStart === -1) throw new Error('verify.yml has no `leg:` matrix under `quality-tests`.');
  // The block ends at the next job-level key (`steps:`, `timeout-minutes:`),
  // which sits at a shallower indent than the matrix items.
  const rest = source.slice(matrixStart);
  const endMatch = rest.search(/\n {4}[a-z][\w-]*:/);
  const block = endMatch === -1 ? rest : rest.slice(0, endMatch + 1);

  const legs = [];
  // Deliberately strict about shape, tolerant about indentation width: each
  // entry must be a `- label: X` line followed by `args: "Y"` and
  // `browser: "Z"` lines, and anything else in the block is a hard error
  // rather than a silent skip — a leg this gate cannot read is a leg whose
  // coverage nobody can prove.
  let pending = null;
  const labelOf = (line) => line.match(/^\s*- label: (.+)$/);
  const argsOfLine = (line) => line.match(/^\s*args: "(.*)"$/);
  const browserOfLine = (line) => line.match(/^\s*browser: "(.*)"$/);
  const commit = () => {
    if (!pending) return;
    if (pending.args === null) throw new Error(`Vitest leg "${pending.label}" has no args line.`);
    legs.push({ ...pending, ...parseArgs(pending.args) });
    pending = null;
  };
  for (const line of block.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed === 'leg:') continue;
    if (trimmed.startsWith('#')) continue;
    const label = labelOf(line);
    if (label) {
      commit();
      pending = { label: label[1], args: null, browser: 'false' };
      continue;
    }
    const args = argsOfLine(line);
    if (args) {
      if (!pending) throw new Error(`An args line has no preceding - label line:\n  ${line}`);
      pending.args = args[1];
      continue;
    }
    const browser = browserOfLine(line);
    if (browser) {
      if (!pending) throw new Error(`A browser line has no preceding - label line:\n  ${line}`);
      pending.browser = browser[1];
      continue;
    }
    if (/^\s*(fail-fast|matrix|strategy):/.test(line)) continue;
    throw new Error(
      `The \`quality-tests\` matrix has a line this gate does not understand:\n  ${line}\n` +
        'It parses that block directly, so keep each leg entry as exactly three lines: ' +
        '`- label: <name>`, `args: "<vitest arguments>"`, and `browser: "true|false"`.',
    );
  }
  commit();
  if (legs.length === 0) throw new Error('No legs found under the `quality-tests` matrix.');
  return legs;
}

/**
 * The vitest arguments split into the part that decides *which* suite runs and
 * the shard pair. `--pool=threads` is a pool choice, not a filter, and is
 * asserted separately by the workflow's own comment (#1257).
 */
function parseArgs(args) {
  const tokens = args.split(/\s+/).filter(Boolean);
  const shardToken = tokens.find((token) => token.startsWith('--shard='));
  const suites = tokens.filter((token) => !token.startsWith('-'));
  const shardMatch = shardToken?.match(/^--shard=(\d+)\/(\d+)$/);
  return {
    shard: shardMatch ? { index: Number(shardMatch[1]), of: Number(shardMatch[2]) } : null,
    suites,
  };
}
