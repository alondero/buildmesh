#!/usr/bin/env node
// Gate the Rust test sharding in `.github/workflows/verify.yml` (issue #1520).
//
// CI validates the exact-module shard plan against the compiled library tests.

// Usage:
//   node scripts/check-rust-shard-coverage.mjs              # lists the tests itself
//   node scripts/check-rust-shard-coverage.mjs --list-file <path>
//
// Exit codes: 0 the shards cover every unit test exactly once; 1 they do not.

import fs from 'node:fs';
import path from 'node:path';
import { listCargoUnitTests, listedUnitTests, partitionUnitTests, readShards, repoRoot } from './ci/rust-shards.mjs';

const argv = process.argv.slice(2);
const listFileIndex = argv.indexOf('--list-file');
const listFile = listFileIndex === -1 ? null : argv[listFileIndex + 1];

try {
  const shards = readShards();
  const tests = listFile
    ? listedUnitTests(fs.readFileSync(listFile, 'utf8'))
    : listCargoUnitTests(path.join(repoRoot, 'src-tauri'));
  const planned = partitionUnitTests(shards, tests);
  console.log(`Rust unit tests: ${tests.length}`);
  for (const [label, names] of planned) {
    console.log(`  ${label.padEnd(24)} ${names.length}`);
  }
  console.log('Rust shard coverage: every unit test is assigned once by its first path segment.');
} catch (error) {
  console.error(`::error::${error.message}`);
  process.exitCode = 1;
}
