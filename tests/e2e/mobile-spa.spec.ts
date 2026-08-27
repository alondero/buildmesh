/**
 * Mobile SPA end-to-end smoke tests.
 *
 * Spawns the built `buildmesh.exe` once and drives Chromium through the
 * QR-pairing flow. Three tests share a single browser context; each
 * `test()` uses `context.clearCookies()` to get a fresh cookie jar rather
 * than respawning the native exe (3 native spawns → ~30s of redundant
 * bind/inspect overhead per spec run).
 *
 * Pre-flight (per CLAUDE.local.md): the stable hub MUST be paused before
 * this spec — the spawned exe binds 1991 (test server) + 1992-1994
 * (mobile SPA), the same ports the stable hub owns. We probe 1991 up
 * front so a violation surfaces as a clear failure rather than a vague
 * spawn error.
 *
 * The bm_session cookie carries a freshly minted per-device token
 * (issue #502) — NOT the root token. The cookie assertion checks
 * value-not-root + shape + all three attributes (HttpOnly, SameSite,
 * Path). The URL assertion waits for NodeList to render first so it
 * doesn't race `Connect.tsx`'s `replaceState` strip (#500).
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

let proc: BuildmeshProcess | null = null;
let mobilePort: number | null = null;

test.describe('mobile /v2 SPA', () => {
  test.use({ viewport: { width: 390, height: 844 } }); // iPhone 14-ish

  // The pre-flight probes 1991 (the test-server port). Per CLAUDE.local.md,
  // the stable hub MUST be paused before this spec runs — otherwise the
  // spawned exe can't bind 1991 and the spawn silently falls back to talking
  // to the hub's DB.
  //
  // We use `test.skip` (not `expect(...).toBe(false)`) so a running hub is a
  // clean SKIP, not a loud FAIL. The spec then doesn't crash, doesn't kill
  // anything, and the user can see in the run output exactly why it was
  // deferred: "skip — port 1991 must be free before this spec …".
  test.beforeAll(async () => {
    test.skip(
      await isPortBound(TEST_SERVER_PORT),
      `port ${TEST_SERVER_PORT} is bound (likely the stable hub) — pause it first per CLAUDE.local.md before this spec can run. The skip is intentional: this spec binds the same ports the hub owns.`,
    );

    proc = spawnBuildmesh();

    expect(
      await waitForPort('127.0.0.1', TEST_SERVER_PORT, 15000),
      'test server ready',
    ).toBe(true);
    mobilePort = await findMobilePort();
    expect(mobilePort, 'one of 1992-1994 should be bound').not.toBeNull();
  });

  test.afterAll(async () => {
    if (proc) {
      await terminate(proc);
      proc = null;
      mobilePort = null;
    }
  });

  test('initial load with ?token= strips the token from URL, lands on NodeList, and sets bm_session cookie', async ({
    page,
    context,
  }) => {
    const { token } = await invokeViaHttp<{ token: string }>('get_root_token');
    expect(token, 'root token shape').toMatch(/^[0-9a-f]{32}$/);

    await page.goto(
      `http://127.0.0.1:${mobilePort}/?token=${encodeURIComponent(token)}`,
    );

    // Wait for the authenticated view BEFORE checking the URL.
    // Connect.tsx strips `?token=` via `replaceState` once login
    // completes — same transition the UI assertion guards.
    // Asserting the URL first would race the strip (issue #500).
    await expect(
      page.getByTestId('node-list').or(page.getByTestId('nodelist-loading')),
    ).toBeVisible({ timeout: 10000 });
    await expect(page.getByTestId('connect-screen')).toBeHidden();

    // After the login transition the URL must be the bare origin —
    // Connect.tsx:27-33 strips `?token=` and rewrites to `<path>`. The
    // web-first `toHaveURL` retries automatically against async
    // `history` mutations; a sync `expect(page.url()).toMatch` would
    // race the same transition.
    await expect(page).toHaveURL(`http://127.0.0.1:${mobilePort}/`);

    // bm_session cookie: HttpOnly + SameSite=Lax + Path=/, holds a
    // *fresh per-device* token (issue #502) — NOT the root token we
    // POSTed. If the cookie ever held the root token again, that
    // shared-credential regression #502 removed would be back.
    const cookies = await context.cookies(`http://127.0.0.1:${mobilePort}`);
    const session = cookies.find((c) => c.name === 'bm_session');
    expect(session, 'bm_session cookie set').toBeDefined();
    expect(session!.value, 'cookie must NOT hold the root token (#502)').not.toBe(token);
    expect(session!.value, 'cookie must be a 32-char lowercase hex token').toMatch(
      /^[0-9a-f]{32}$/,
    );
    expect(session!.httpOnly, 'cookie HttpOnly').toBe(true);
    expect(session!.sameSite, 'cookie SameSite=Lax').toBe('Lax');
    expect(session!.path, 'cookie Path=/').toBe('/');
  });

  test('reload without ?token= reuses bm_session and lands on NodeList', async ({
    page,
  }) => {
    const { token } = await invokeViaHttp<{ token: string }>('get_root_token');
    await page.goto(
      `http://127.0.0.1:${mobilePort}/?token=${encodeURIComponent(token)}`,
    );
    await expect(
      page.getByTestId('node-list').or(page.getByTestId('nodelist-loading')),
    ).toBeVisible({ timeout: 10000 });

    // Reload without `?token=`; the cookie alone should re-auth.
    await page.goto(`http://127.0.0.1:${mobilePort}/`);
    await expect(
      page.getByTestId('node-list').or(page.getByTestId('nodelist-loading')),
    ).toBeVisible({ timeout: 10000 });
    await expect(page.getByTestId('connect-screen')).toBeHidden();
    // URL stays at `/` on reload — the cookie handles auth.
    await expect(page).toHaveURL(`http://127.0.0.1:${mobilePort}/`);
  });

  test('cold load (no ?token=, no cookie) renders Connect screen', async ({
    page,
    context,
  }) => {
    // Ensure no cookies carry over from prior tests.
    await context.clearCookies();

    await page.goto(`http://127.0.0.1:${mobilePort}/`);
    await expect(page.getByTestId('connect-screen')).toBeVisible({ timeout: 10000 });
    await expect(page).toHaveURL(`http://127.0.0.1:${mobilePort}/`);
  });
});
