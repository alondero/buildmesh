import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { BRIDGE_TOKEN_ENV } from '../../scripts/test-bridge-auth.mjs';
import { profileOwnerIsAlive, removeSpawnDataRoot } from '../e2e/utils/buildmesh-launcher';
import { headersForProbe } from '../e2e/utils/tauri-http';

const TOKEN = 'a'.repeat(32);

describe('e2e profile and bridge probe guards', () => {
  const scratch: string[] = [];

  afterEach(() => {
    for (const dir of scratch.splice(0)) {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it('attaches the bearer only to the loopback bridge', () => {
    expect(headersForProbe('192.168.1.20', 1992)).toBeUndefined();
    expect(headersForProbe('127.0.0.1', 1992)).toBeUndefined();
    expect(headersForProbe('127.0.0.1', 1994)).toBeUndefined();
    expect(headersForProbe('10.0.0.8', 1991)).toBeUndefined();

    const previous = process.env[BRIDGE_TOKEN_ENV];
    process.env[BRIDGE_TOKEN_ENV] = TOKEN;
    try {
      expect(headersForProbe('127.0.0.1', 1991)?.Authorization).toBe(`Bearer ${TOKEN}`);
      expect(headersForProbe('localhost', 1991)?.Authorization).toBe(`Bearer ${TOKEN}`);
      expect(headersForProbe('[::1]', 1991)?.Authorization).toBe(`Bearer ${TOKEN}`);
    } finally {
      if (previous === undefined) delete process.env[BRIDGE_TOKEN_ENV];
      else process.env[BRIDGE_TOKEN_ENV] = previous;
    }
  });

  it('treats a live instance-owner pid as the hub and ignores a dead one', async () => {
    const dir = mkdtempSync(path.join(tmpdir(), 'bm-owner-pid-'));
    scratch.push(dir);
    const pidFile = path.join(dir, 'instance-owner.pid');
    expect(profileOwnerIsAlive(dir)).toBe(false);
    writeFileSync(pidFile, '0');
    expect(profileOwnerIsAlive(dir)).toBe(false);
    writeFileSync(pidFile, 'not-a-pid');
    expect(profileOwnerIsAlive(dir)).toBe(false);

    const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' });
    writeFileSync(pidFile, String(child.pid));
    expect(profileOwnerIsAlive(dir)).toBe(true);
    child.kill();
    await new Promise<void>((resolve) => child.once('exit', () => resolve()));
    expect(profileOwnerIsAlive(dir)).toBe(false);
  });

  it('removes only a spawn temp root under the OS temp directory', () => {
    const owned = mkdtempSync(path.join(tmpdir(), 'buildmesh-e2e-guard-'));
    const kept = mkdtempSync(path.join(tmpdir(), 'bm-not-a-spawn-'));
    scratch.push(kept);
    writeFileSync(path.join(owned, 'buildmesh.db'), 'x');
    writeFileSync(path.join(kept, 'buildmesh.db'), 'x');

    removeSpawnDataRoot(owned);
    expect(existsSync(owned)).toBe(false);
    removeSpawnDataRoot(kept);
    expect(existsSync(path.join(kept, 'buildmesh.db'))).toBe(true);
    removeSpawnDataRoot(path.join(kept, '..', '..'));
    expect(existsSync(kept)).toBe(true);
  });
});
