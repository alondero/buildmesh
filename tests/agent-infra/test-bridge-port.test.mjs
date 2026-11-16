import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import * as bridge from '../../scripts/test-bridge-auth.mjs';

test('bridge calls use the recorded worktree offset and preserve explicit overrides', t => {
  const root = mkdtempSync(join(tmpdir(), 'bridge-port-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const record = join(root, 'dev-instance.json');
  writeFileSync(record, JSON.stringify({ portOffset: 1700 }));
  assert.equal(bridge.bridgePort(undefined, record), 3691);
  assert.equal(bridge.bridgePort('4991', record), 4991);
  writeFileSync(record, JSON.stringify({ portOffset: null }));
  assert.equal(bridge.bridgePort(undefined, record), 2991);
});

test('missing or malformed instance information fails before contacting an assumed bridge', t => {
  const root = mkdtempSync(join(tmpdir(), 'bridge-port-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const record = join(root, 'dev-instance.json');
  assert.throws(() => bridge.bridgePort(undefined, record), /launch|instance/i);
  assert.equal(bridge.bridgePort(4991, record), 4991);
  for (const contents of ['bad JSON', '{}', '{"portOffset":"1700"}', '{"portOffset":-3000}', '{"portOffset":64000}']) {
    writeFileSync(record, contents);
    assert.throws(() => bridge.bridgePort(undefined, record), /instance|offset|port/i, contents);
  }
  for (const port of ['', 'abc', '0', '-1', '65536', '1.5']) {
    assert.throws(() => bridge.bridgePort(port, record), /port/i, port);
  }
});
