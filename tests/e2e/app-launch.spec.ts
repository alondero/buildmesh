/**
 * E2E App Launch Tests (Built Exe)
 *
 * Tests that verify the BUILT exe (from `npm run tauri build`) works correctly.
 * This is separate from dev-mode tests because the built exe:
 * - Has frontend embedded (no Vite dev server needed)
 * - Serves HTTP test commands on port 1991
 * - Does NOT serve on port 1420
 *
 * CRITICAL: These tests will FAIL if the app was built with `cargo build`
 * instead of `npm run tauri build`. The latter embeds the frontend.
 *
 * Run with: npx playwright test tests/e2e/app-launch.spec.ts
 */
import { test, expect } from '@playwright/test';
import { spawn } from 'child_process';
import fs from 'fs';
import path from 'path';
import { exec } from 'child_process';
import util from 'util';

const execPromise = util.promisify(exec);

const EXE_PATH = 'X:/src/buildmesh/src-tauri/target/release/buildmesh.exe';
const DIST_PATH = 'X:/src/buildmesh/dist';

async function killAllBuildmeshProcesses() {
  try {
    await execPromise('taskkill /IM buildmesh.exe /F');
  } catch {
    // Ignore errors
  }
}

async function waitForPort(port: number, timeoutMs: number = 10000): Promise<boolean> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/health`);
      if (response.ok) return true;
    } catch {
      // Not ready yet
    }
    await new Promise(r => setTimeout(r, 200));
  }
  return false;
}

test.describe('built exe app launch', () => {

  test.beforeEach(async () => {
    await killAllBuildmeshProcesses();
    await new Promise(r => setTimeout(r, 500));
  });

  test.afterEach(async () => {
    await killAllBuildmeshProcesses();
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
    // A proper tauri build produces an exe with embedded assets (several MB)
    expect(stats.size, 'Release exe should be several MB with embedded frontend').toBeGreaterThan(1024 * 1024);
  });

  test('release exe is not using debug build', async () => {
    // This test catches the issue where debug build was running instead of release
    // Debug builds show "can't reach this page" because they expect Vite on port 1420
    const debugExe = 'X:/src/buildmesh/src-tauri/target/debug/buildmesh.exe';
    const debugExists = fs.existsSync(debugExe);

    if (debugExists) {
      const releaseStats = fs.statSync(EXE_PATH);
      const debugStats = fs.statSync(debugExe);

      // If debug is newer, it was likely used instead of release
      expect(debugStats.mtimeMs, 'Debug build is newer than release! Run `npm run tauri build` before testing release.').toBeLessThan(releaseStats.mtimeMs);
    }
  });

  test('built exe starts and HTTP test server responds', async () => {
    // Start the built exe (not npm run tauri dev - just the raw exe)
    const appProcess = spawn(EXE_PATH, [], {
      stdio: 'ignore',
      windowsHide: true,
      detached: true
    });
    appProcess.unref();

    try {
      // Wait for HTTP test server to be ready on port 1991
      const serverReady = await waitForPort(1991, 10000);
      expect(serverReady, 'HTTP test server should be ready on port 1991').toBe(true);

      // Verify we can call a simple command via HTTP
      const response = await fetch('http://127.0.0.1:1991/health');
      expect(response.ok, 'Health endpoint should respond OK').toBe(true);
      expect(response.status, 'Health endpoint status should be 200').toBe(200);

    } finally {
      await killAllBuildmeshProcesses();
    }
  });

  test('built exe can create a project via HTTP', async () => {
    const appProcess = spawn(EXE_PATH, [], {
      stdio: 'ignore',
      windowsHide: true,
      detached: true
    });
    appProcess.unref();

    try {
      // Wait for server
      const serverReady = await waitForPort(1991, 10000);
      expect(serverReady).toBe(true);

      // Call create_test_mesh via HTTP
      const response = await fetch('http://127.0.0.1:1991/invoke', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          cmd: 'create_test_mesh',
          args: { name: 'Launch Test Project' }
        })
      });

      expect(response.ok, 'HTTP invoke should succeed').toBe(true);
      const json = await response.json() as { ok: boolean; data?: { id: number } };
      expect(json.ok, 'Command should succeed').toBe(true);
      expect(json.data?.id, 'Project should have an ID').toBeGreaterThan(0);

    } finally {
      await killAllBuildmeshProcesses();
    }
  });
});
