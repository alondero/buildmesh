#!/usr/bin/env node
// Gate the Rust test sharding in `.github/workflows/verify.yml` (issue #1520).
//
// The `rust-tests` matrix splits the unit target with libtest substring
// filters, and substring filters cannot express "this test's first path
// segment is X". So the split is a list of exact filters and `--skip`s, and
// nothing but this script keeps it honest: the moment a new top-level module
// appears and no shard claims it, its tests stop running silently. That is a
// worse failure than the runner loss the sharding was added to diagnose, so it
// gets its own gate.
//
// The matrix is parsed strictly by scripts/ci/rust-shards.mjs, which the local
// shard runner (scripts/rust-test-shards.mjs) shares, so CI and local runs
// split the suite identically.
//
// Usage:
//   node scripts/check-rust-shard-coverage.mjs              # lists the tests itself
//   node scripts/check-rust-shard-coverage.mjs --list-file <path>
//
// Exit codes: 0 the shards cover every unit test exactly once; 1 they do not.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { readShards, repoRoot } from './ci/rust-shards.mjs';

const argv = process.argv.slice(2);
const listFileIndex = argv.indexOf('--list-file');
const listFile = listFileIndex === -1 ? null : argv[listFileIndex + 1];

function fail(message) {
  console.error(`::error::${message}`);
  process.exit(1);
}

// --- the shard matrix, read out of the workflow (shared with the local runner)
function readWorkflowShards() {
  try {
    return readShards();
  } catch (error) {
    fail(error.message);
  }
}

// libtest: positive filters are OR'd substrings; `--skip X` takes X as the
// next argv item and removes any test whose name contains it.
function parseArgs(args) {
  const filters = [];
  const skips = [];
  const tokens = args.split(/\s+/).filter(Boolean);
  for (let i = 0; i < tokens.length; i++) {
    if (tokens[i] === '--skip') {
      const value = tokens[i + 1];
      if (value === undefined) fail(`A \`--skip\` with no pattern in: ${args}`);
      skips.push(value);
      i++;
    } else {
      filters.push(tokens[i]);
    }
  }
  return { filters, skips };
}

// --- the unit tests, listed by the test binary itself ------------------------
function readTests() {
  const raw = listFile
    ? fs.readFileSync(path.resolve(listFile), 'utf8')
    : execFileSync('cargo', ['test', '--locked', '--lib', '--', '--list'], {
        cwd: path.join(repoRoot, 'src-tauri'),
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'inherit'],
        shell: process.platform === 'win32',
      });
  return raw
    .split(/\r?\n/)
    .filter((line) => line.endsWith(': test'))
    .map((line) => line.replace(/^test\s+/, '').replace(/: test$/, ''));
}

const shards = readWorkflowShards();
const tests = readTests();
if (tests.length === 0) fail('`cargo test --lib -- --list` returned no tests; cannot verify coverage.');

const claims = new Map();
for (const shard of shards) {
  const { filters, skips } = parseArgs(shard.args);
  for (const name of tests) {
    if (skips.some((skip) => name.includes(skip))) continue;
    const selected = filters.length === 0 || filters.some((filter) => name.includes(filter));
    if (!selected) continue;
    if (!claims.has(name)) claims.set(name, []);
    claims.get(name).push(shard.label);
  }
}

const uncovered = tests.filter((name) => !claims.has(name));
const duplicated = [...claims.entries()].filter(([, labels]) => labels.length > 1);

console.log(`Rust unit tests: ${tests.length}`);
for (const shard of shards) {
  const count = [...claims.values()].filter((labels) => labels.includes(shard.label)).length;
  console.log(`  ${shard.label.padEnd(24)} ${String(count).padStart(5)}`);
}

if (duplicated.length > 0) {
  console.error('');
  for (const [name, labels] of duplicated.slice(0, 10)) {
    console.error(`  claimed by ${labels.join(' and ')}: ${name}`);
  }
  fail(
    `${duplicated.length} unit test(s) are claimed by more than one shard. libtest filters are ` +
      'substring matches, so a nested path like `models::agent::` matches an `agent::` filter too. ' +
      'Add a `--skip <prefix>::` to the narrower shard.',
  );
}

if (uncovered.length > 0) {
  const modules = [...new Set(uncovered.map((name) => name.split('::')[0]))].sort();
  console.error('');
  for (const name of uncovered.slice(0, 10)) console.error(`  not run by any shard: ${name}`);
  fail(
    `${uncovered.length} unit test(s) are not run by any shard (top-level modules: ${modules.join(', ')}). ` +
      'Add the module to the `remaining` shard\'s filters, or give it its own shard — otherwise its ' +
      'tests stop running in CI without anything failing.',
  );
}

console.log('Rust shard coverage: every unit test is claimed by exactly one shard.');
