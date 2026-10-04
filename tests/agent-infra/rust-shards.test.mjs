import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readShards } from '../../scripts/ci/rust-shards.mjs';

test('the shard list is read from the real verify workflow', () => {
  const shards = readShards();
  assert.deepEqual(shards.map((shard) => shard.label), [
    'db',
    'services',
    'agent',
    'commands-http',
    'circuit-coordinator',
    'git-env-preferences',
    'remaining',
  ]);
  assert.equal(shards.find((shard) => shard.label === 'db').args, 'db::');
});

test('a matrix line the parser does not understand fails loudly instead of dropping a shard', () => {
  const workflow = [
    '  rust-tests:',
    '    strategy:',
    '      matrix:',
    '        shard:',
    '          - label: db',
    '            args: "db::"',
    '          - label: broken',
    '            filters: "services::"',
    '    steps:',
  ].join('\n');
  assert.throws(() => readShards(workflow), /does not understand/);
  assert.throws(() => readShards('jobs:\n  quality:\n'), /no `rust-tests` job/);
});
