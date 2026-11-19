import { spawnSync } from 'node:child_process';
import { exactTestBatches, listCargoUnitTests, partitionUnitTests, readShards } from './rust-shards.mjs';

const label = process.argv[2];
try {
  const shards = readShards();
  if (!shards.some(shard => shard.label === label)) throw new Error(`Unknown Rust shard: ${label}`);
  const names = partitionUnitTests(shards, listCargoUnitTests()).get(label);
  console.log(`Rust shard ${label}: ${names.length} assigned tests`);
  for (const batch of exactTestBatches(names)) {
    const result = spawnSync('cargo', ['test', '--locked', '--lib', '--', '--exact', ...batch], { stdio: 'inherit' });
    if (result.error) throw result.error;
    if (result.status !== 0 && !process.exitCode) process.exitCode = result.status ?? 1;
  }
} catch (error) {
  console.error(`::error::${error.message}`);
  process.exitCode = 1;
}
