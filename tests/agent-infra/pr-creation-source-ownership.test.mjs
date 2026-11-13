import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

import {
  commandLayerReaches,
  commandNames,
  isPrCreateCall,
  nodeResolverViolations,
  prCreationSourceViolations,
  removedCommandViolations,
  rustFunctions,
  SOURCE_RESOLVERS,
  sourceOwnershipViolations,
} from '../../scripts/check-pr-creation-source-ownership.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');

test('the guard reads the function a call sits in, not the whole file', () => {
  const code = [
    'fn outer() {',
    '  let x = 1;',
    '}',
    'fn creates() {',
    '  client.create_pull_request_idempotent(req);',
    '}',
  ].join('\n');
  const names = rustFunctions(code).map((fn) => fn.name);
  assert.deepEqual(names, ['outer', 'creates']);
  const creates = rustFunctions(code)[1];
  assert.ok(creates.body.includes('create_pull_request_idempotent'));
  assert.ok(!creates.body.includes('let x = 1'), 'a sibling body must not leak in');
});

test('the guard sees a call through a path or a method', () => {
  assert.ok(isPrCreateCall('client.create_pull_request_idempotent(req);'));
  assert.ok(isPrCreateCall('GitHubClient::create_pull_request_idempotent(req);'));
  assert.ok(
    isPrCreateCall('client\n  .create_pull_request_idempotent(req);'),
    'a method call split over two lines is still a call',
  );
  // The client's own definition of the method is not a call site.
  assert.ok(!isPrCreateCall('pub fn create_pull_request_idempotent(req: Req) {}'));
});

test('the guard reads commands, not DTOs', () => {
  const source = [
    '/// A wire shape, not a command.',
    'pub struct GitHubIssue { pub number: i64 }',
    '#[command]',
    'pub async fn create_pr_for_node_source(mesh_id: i64) -> u8 { mesh_id }',
  ].join('\n');
  const names = commandNames(source);
  assert.ok(names.has('create_pr_for_node_source'));
  assert.ok(!names.has('GitHubIssue'));
});

test('no service or circuit module reaches the PR command layer', () => {
  const reaches = commandLayerReaches(repoRoot);
  assert.deepEqual(reaches, [], reaches.join('\n'));
});

test('every production function that opens a pull request names its source resolver', () => {
  const violations = sourceOwnershipViolations(repoRoot);
  assert.deepEqual(violations, [], violations.join('\n'));
});

test('the node-scoped resolver reads the agent worktree, not the mesh root', () => {
  const violations = nodeResolverViolations(repoRoot);
  assert.deepEqual(violations, [], violations.join('\n'));
});

test('the project-root create-PR commands stay removed', () => {
  const violations = removedCommandViolations(repoRoot);
  assert.deepEqual(violations, [], violations.join('\n'));
});

test('the source resolvers the guard accepts are the ones the crate actually uses', () => {
  // A resolver the guard names but the crate does not have would let a
  // broken path pass by naming a function that does not exist.
  const names = new Set(SOURCE_RESOLVERS.map((resolver) => resolver.name));
  assert.ok(names.has('resolve_pr_source_for_node'));
  assert.ok(names.has('observe_wrapup_git_state'));
});

test('the guard\'s combined report is empty on the candidate', () => {
  const violations = prCreationSourceViolations(repoRoot);
  assert.deepEqual(violations, [], violations.join('\n'));
});
