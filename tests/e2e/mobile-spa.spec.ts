/**
 * Mobile SPA end-to-end smoke tests.
 *
 * Launches the built buildmesh.exe, asks it for the root token via the
 * test server (port 1991), then loads /?token=... in Chromium at a
 * phone-sized viewport and asserts that the Connect screen redirects
 * straight to the node list (since the URL token validates and the
 * server sets the bm_session cookie).
 *
 * Prereqs:
 *   - npm run build          (produces dist/ AND dist/mobile/)
 *   - npm run tauri build    (produces the release exe with assets embedded)
 *
 * Run: npx playwright test tests/e2e/mobile-spa.spec.ts
 */
import { test, expect } from '@playwright/test';
import { spawn } from 'child_process';
import { exec } from 'child_process';
import util from 'util';

const execPromise = util.promisify(exec);

const EXE_PATH = 'X:/src/buildmesh/src-tauri/target/release/buildmesh.exe';

async function killAllBuildmeshProcesses() {
  try {
    await execPromise('taskkill /IM buildmesh.exe /F');
  } catch {
    // Ignore — nothing to kill is the success case.
  }
}

async function waitForPort(port: number, timeoutMs = 10000): Promise<boolean> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/health`);
      if (response.ok) return true;
    } catch {
      // Not ready yet.
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}

async function findMobilePort(): Promise<number | null> {
  // The mobile server tries 1992 → 1993 → 1994.
  for (const port of [1992, 1993, 1994]) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/`, { redirect: 'manual' });
      // Any response (even 401) tells us something is bound here.
      if (r.status > 0) return port;
    } catch {
      // try next
    }
  }
  return null;
}

async function fetchRootToken(): Promise<string> {
  const r = await fetch('http://127.0.0.1:1991/invoke', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ cmd: 'get_root_token', args: {} }),
  });
  const j = (await r.json()) as {
    ok: boolean;
    data?: { token: string };
    error?: string;
  };
  if (!j.ok || !j.data?.token) {
    throw new Error('get_root_token failed: ' + (j.error ?? 'unknown'));
  }
  return j.data.token;
}

test.describe('mobile /v2 SPA', () => {
  test.use({ viewport: { width: 390, height: 844 } }); // iPhone 14-ish

  test.beforeEach(async () => {
    await killAllBuildmeshProcesses();
    await new Promise((r) => setTimeout(r, 500));
  });

  test.afterEach(async () => {
    await killAllBuildmeshProcesses();
  });

  test('initial load redirects from token to node list and sets bm_session cookie', async ({
    page,
    context,
  }) => {
    const child = spawn(EXE_PATH, [], {
      stdio: 'ignore',
      windowsHide: true,
      detached: true,
    });
    child.unref();

    expect(await waitForPort(1991, 15000), 'test server ready').toBe(true);
    const mobilePort = await findMobilePort();
    expect(mobilePort, 'one of 1992-1994 should be bound').not.toBeNull();

    const token = await fetchRootToken();
    expect(token).toMatch(/^[0-9a-f]{32}$/);

    await page.goto(
      `http://127.0.0.1:${mobilePort}/?token=${encodeURIComponent(token)}`,
    );

    // After App.tsx replaces history, the token should no longer be in the URL.
    await expect(page).toHaveURL(/\/?$/);

    // The node list (empty or populated) renders, not the connect screen.
    await expect(
      page.getByTestId('node-list').or(page.getByTestId('nodelist-loading')),
    ).toBeVisible({ timeout: 10000 });
    await expect(page.getByTestId('connect-screen')).toBeHidden();

    // bm_session cookie is set HttpOnly + SameSite=Lax + Path=/
    const cookies = await context.cookies();
    const session = cookies.find((c) => c.name === 'bm_session');
    expect(session, 'bm_session cookie set').toBeDefined();
    expect(session!.value).toBe(token);
    expect(session!.httpOnly).toBe(true);
  });

  test('request without token gets 401', async () => {
    const child = spawn(EXE_PATH, [], {
      stdio: 'ignore',
      windowsHide: true,
      detached: true,
    });
    child.unref();

    expect(await waitForPort(1991, 15000)).toBe(true);
    const mobilePort = await findMobilePort();
    expect(mobilePort).not.toBeNull();

    const r = await fetch(`http://127.0.0.1:${mobilePort}/`);
    expect(r.status).toBe(401);
  });
});
