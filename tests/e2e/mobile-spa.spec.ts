/**
 * Mobile SPA end-to-end smoke tests.
 *
 * Each test spawns a fresh `buildmesh.exe`, drives Chromium at a phone-sized
 * viewport through the real SPA, and asserts the QR-pairing flow.
 *
 * Pre-flight (per CLAUDE.local.md): the stable hub MUST be paused before this
 * spec — the spawned exe binds 1991 (test server) + 1992-1994 (mobile SPA),
 * the same ports the stable hub owns. We probe 1991 up front so a violation
 * surfaces as a clear failure rather than a vague "bind" error.
 *
 * The bm_session cookie carries a freshly minted per-device token (issue
 * #502) — NOT the root token. So we assert the cookie value is a different
 * 32-char hex token, and that it survives a navigation that strips `?token=`
 * from the URL (issue #500).
 *
 * Prereqs:
 *   - npm run build          (produces dist/ AND dist/mobile/)
 *   - npm run tauri build    (produces the release exe with assets embedded)
 *
 * Override the exe path if the build isn't at the default location:
 *   BUILDMESH_EXE=/abs/path/to/buildmesh.exe npx playwright test …
 *
 * Run: npx playwright test tests/e2e/mobile-spa.spec.ts
 */
import { test, expect } from '@playwright/test';
import { invokeViaHttp, waitForPort } from './utils/tauri-http';
import {
  spawnBuildmesh,
  terminate,
  findMobilePort,
  isPortBound,
  TEST_SERVER_PORT,
  type BuildmeshProcess,
} from './utils/buildmesh-launcher';

// Process we last spawned, if any. Tracked by PID so cleanup can target
// *that* process instead of falling back to `taskkill /IM buildmesh.exe /F`
// — an image-name sledgehammer that would also murder the user's stable hub
// if it happened to be running.
let proc: BuildmeshProcess | null = null;

test.describe('mobile /v2 SPA', () => {
  test.use({ viewport: { width: 390, height: 844 } }); // iPhone 14-ish

  test.beforeEach(async () => {
    // Belt-and-braces: if a previous crashed run left a zombie on the test
    // server, terminate it before we spawn a new one. terminate() is a
    // no-op when the port is free.
    if (proc) {
      await terminate(proc);
      proc = null;
    }
    // Hard gate: the test server port must be free. The user can satisfy
    // this by closing the stable hub (CLAUDE.local.md) or by setting
    // BUILDMESH_EXE to a dev-profile exe that uses 2991 instead.
    expect(
      await isPortBound(TEST_SERVER_PORT),
      `port ${TEST_SERVER_PORT} must be free before this spec — pause the stable hub first (see CLAUDE.local.md)`,
    ).toBe(false);
  });

  test.afterEach(async () => {
    if (proc) {
      await terminate(proc);
      proc = null;
    }
  });

  test('initial load with ?token= lands on NodeList, sets bm_session cookie, and strips token from URL', async ({
    page,
    context,
  }) => {
    proc = spawnBuildmesh();
    expect(
      await waitForPort('127.0.0.1', TEST_SERVER_PORT, 15000),
      'test server ready',
    ).toBe(true);
    const mobilePort = await findMobilePort();
    expect(mobilePort, 'one of 1992-1994 should be bound').not.toBeNull();

    const { token } = await invokeViaHttp<{ token: string }>('get_root_token');
    expect(token, 'root token shape').toMatch(/^[0-9a-f]{32}$/);

    await page.goto(
      `http://127.0.0.1:${mobilePort}/?token=${encodeURIComponent(token)}`,
    );

    // Wait for the authenticated view BEFORE checking the URL — Connect.tsx
    // strips `?token=` via `replaceState` once login completes, so the URL
    // assertion depends on the same transition the UI assertion guards.
    // Asserting the URL first would race the `replaceState` (issue #500).
    await expect(
      page.getByTestId('node-list').or(page.getByTestId('nodelist-loading')),
    ).toBeVisible({ timeout: 10000 });
    await expect(page.getByTestId('connect-screen')).toBeHidden();

    // After Connect.tsx strips the token, the URL must no longer carry
    // `?token=`. We also assert we landed on our mobile origin (not
    // about:blank or a 404) — a purely-negative `not.toHaveURL(/token=/)`
    // would pass on any off-path landing.
    expect(page.url(), 'URL must be the mobile SPA origin').toMatch(
      new RegExp(`^http://127\\.0\\.0\\.1:${mobilePort}/(\\?.*)?$`),
    );
    expect(page.url(), 'token must NOT linger in the URL (#500)').not.toMatch(/token=/);

    // bm_session cookie: HttpOnly + SameSite=Lax + Path=/, holds a
    // *fresh per-device* token (issue #502) — NOT the root token we
    // POSTed. If the cookie ever held the root token again, the
    // shared-credential regression #502 removed would be back.
    const cookies = await context.cookies(`http://127.0.0.1:${mobilePort}`);
    const session = cookies.find((c) => c.name === 'bm_session');
    expect(session, 'bm_session cookie set').toBeDefined();
    expect(
      session!.value,
      'cookie must NOT hold the root token (#502)',
    ).not.toBe(token);
    expect(
      session!.value,
      'cookie must be a 32-char lowercase hex token',
    ).toMatch(/^[0-9a-f]{32}$/);
    expect(session!.httpOnly, 'cookie HttpOnly').toBe(true);
    expect(session!.sameSite, 'cookie SameSite=Lax').toBe('Lax');
    expect(session!.path, 'cookie Path=/').toBe('/');
  });

  test('reload without ?token= reuses bm_session and lands on NodeList', async ({
    page,
    context,
  }) => {
    proc = spawnBuildmesh();
    expect(await waitForPort('127.0.0.1', TEST_SERVER_PORT, 15000)).toBe(true);
    const mobilePort = await findMobilePort();
    expect(mobilePort).not.toBeNull();

    // First visit: pair via ?token=, which sets the bm_session cookie.
    const { token } = await invokeViaHttp<{ token: string }>('get_root_token');
    await page.goto(
      `http://127.0.0.1:${mobilePort}/?token=${encodeURIComponent(token)}`,
    );
    await expect(
      page.getByTestId('node-list').or(page.getByTestId('nodelist-loading')),
    ).toBeVisible({ timeout: 10000 });

    // Second visit: same browser context (cookies preserved), no ?token=.
    // The cookie is the per-device token from #502, so the SPA must
    // re-authenticate via the cookie and land on the list — not bounce
    // back to the Connect screen.
    await page.goto(`http://127.0.0.1:${mobilePort}/`);
    await expect(
      page.getByTestId('node-list').or(page.getByTestId('nodelist-loading')),
    ).toBeVisible({ timeout: 10000 });
    await expect(page.getByTestId('connect-screen')).toBeHidden();
  });

  test('cold load (no ?token=, no cookie) renders Connect screen', async ({
    page,
    context,
  }) => {
    proc = spawnBuildmesh();
    expect(await waitForPort('127.0.0.1', TEST_SERVER_PORT, 15000)).toBe(true);
    const mobilePort = await findMobilePort();
    expect(mobilePort).not.toBeNull();

    // Ensure no cookies carry over from prior tests in this describe.
    await context.clearCookies();

    await page.goto(`http://127.0.0.1:${mobilePort}/`);
    await expect(page.getByTestId('connect-screen')).toBeVisible({ timeout: 10000 });
    // URL stays at `/` — no token was ever presented, so nothing to strip.
    expect(page.url(), 'no ?token= ever, so URL is just /').toBe(
      `http://127.0.0.1:${mobilePort}/`,
    );
  });
});
