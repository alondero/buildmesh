/**
 * E2E launch tests for a dev-tools build of the stable profile.
 *
 * A plain `npm run tauri build` does not include the test bridge. These
 * tests need `npx tauri build --features dev-tools` (stable identifier, so
 * the bridge stays on port 1991) and an embedded frontend (`dist/`).
 *
 * The launcher isolates the profile and passes a per-launch credential.
 * Nothing here kills `buildmesh.exe` by image name.
 *
 * Run with: npx playwright test tests/e2e/app-launch.spec.ts
 */
import { test, expect } from '@playwright/test';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { BRIDGE_MISSING_HINT, invokeViaHttp, isTestServerReady, waitForPort } from './utils/tauri-http';
import {
  EXE_PATH,
  isPortBound,
  spawnBuildmesh,
  terminate,
  TEST_SERVER_PORT,
  type BuildmeshProcess,
} from './utils/buildmesh-launcher';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const DIST_PATH = path.join(repoRoot, 'dist');
const DEBUG_EXE = path.join(repoRoot, 'src-tauri', 'target', 'debug', 'buildmesh.exe');

let proc: BuildmeshProcess | null = null;

test.describe('dev-tools exe app launch', () => {
  test.beforeEach(async () => {
    test.skip(
      await isPortBound(TEST_SERVER_PORT),
      `port ${TEST_SERVER_PORT} is already bound — pause the stable hub before this spec. It is not killed by image name.`,
    );
  });

  test.afterEach(async () => {
    if (proc) {
      await terminate(proc);
      proc = null;
    }
  });

  test('dist folder exists and has content', () => {
    expect(fs.existsSync(DIST_PATH), 'dist folder must exist').toBe(true);
    const indexPath = path.join(DIST_PATH, 'index.html');
    expect(fs.existsSync(indexPath), 'dist/index.html must exist').toBe(true);
    const stats = fs.statSync(indexPath);
    expect(stats.size, 'index.html must have content').toBeGreaterThan(100);
  });

  test('exe file exists and has reasonable size', () => {
    expect(fs.existsSync(EXE_PATH), `Release exe must exist at ${EXE_PATH}`).toBe(true);
    const stats = fs.statSync(EXE_PATH);
    expect(stats.size, 'Release exe should be several MB with embedded frontend').toBeGreaterThan(1024 * 1024);
  });

  test('release exe is not using debug build', () => {
    test.skip(
      !fs.existsSync(DEBUG_EXE) || !fs.existsSync(EXE_PATH),
      'both the debug and release binaries are required to compare build times',
    );
    const releaseStats = fs.statSync(EXE_PATH);
    const debugStats = fs.statSync(DEBUG_EXE);
    expect(
      debugStats.mtimeMs,
      'Debug build is newer than release. Rebuild the dev-tools stable profile before this spec.',
    ).toBeLessThan(releaseStats.mtimeMs);
  });

  test('dev-tools exe starts and the authenticated bridge answers', async () => {
    proc = spawnBuildmesh();
    const serverReady = await waitForPort('127.0.0.1', TEST_SERVER_PORT, 15000);
    expect(serverReady, BRIDGE_MISSING_HINT).toBe(true);
    expect(await isTestServerReady(), 'Authenticated /health should return OK').toBe(true);
  });

  test('authenticated bridge can create a mesh in the isolated profile', async () => {
    proc = spawnBuildmesh();
    const serverReady = await waitForPort('127.0.0.1', TEST_SERVER_PORT, 15000);
    expect(serverReady, BRIDGE_MISSING_HINT).toBe(true);

    const project = await invokeViaHttp<{ id: number }>('create_test_mesh', {
      name: 'Launch Test Project',
    });
    expect(project.id, 'Project should have an ID').toBeGreaterThan(0);
  });
});
