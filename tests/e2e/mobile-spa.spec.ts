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
 * this spec — the spawned exe binds 1992-1994 (mobile SPA), the same
 * ports the stable hub owns. The hub does not bind the test bridge, so
 * the guard reads `instance-owner.pid` in the real profile.
 *
 * The bm_session cookie carries a freshly minted per-device token
 * (issue #502) — NOT the root token. The cookie assertion checks
 * value-not-root + shape + all three attributes (HttpOnly, SameSite,
 * Path). The URL assertion waits for NodeList to render first so it
 * doesn't race `Connect.tsx`'s `replaceState` strip (#500).
 *
 * Prereqs:
 *   - npm run build          (produces dist/ AND dist/mobile/)
 *   - npx tauri build --features dev-tools
 *     (stable identifier, embedded assets, test bridge compiled in)
 *
 * Override the exe path if the build isn't at the default location:
 *   BUILDMESH_EXE=/abs/path/to/buildmesh.exe npx playwright test …
 *
 * Run: npx playwright test tests/e2e/mobile-spa.spec.ts
 */
import { test, expect } from '@playwright/test';
import { BRIDGE_MISSING_HINT, invokeViaHttp, waitForPort } from './utils/tauri-http';
import {
  spawnBuildmesh,
  terminate,
  findMobilePort,
  stableHubIsRunning,
  TEST_SERVER_PORT,
  type BuildmeshProcess,
} from './utils/buildmesh-launcher';

let proc: BuildmeshProcess | null = null;
let mobilePort: number | null = null;

test.describe('mobile /v2 SPA', () => {
  test.use({ viewport: { width: 390, height: 844 } }); // iPhone 14-ish

  // The stable hub does not bind port 1991. A live `instance-owner.pid`
  // in the real profile means this spec would collide on 1992–1994.
  // `test.skip` keeps that a clean skip: nothing is killed by image name.
  test.beforeAll(async () => {
    test.skip(
      stableHubIsRunning(),
      'the stable hub is running (instance-owner.pid) — pause it first per CLAUDE.local.md before this spec can run. The skip is intentional: this spec binds the phone/Android ports the hub owns.',
    );

    proc = spawnBuildmesh();

    expect(
      await waitForPort('127.0.0.1', TEST_SERVER_PORT, 15000),
      BRIDGE_MISSING_HINT,
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

  test('initial load with #pair= strips the token from URL, lands on NodeList, and sets bm_session cookie', async ({
    page,
    context,
  }) => {
    const { ticket } = await invokeViaHttp<{ ticket: string }>('create_pairing_ticket');
    expect(ticket, 'pairing ticket shape').toMatch(/^[0-9a-f]{32}$/);

    await page.goto(
      `http://127.0.0.1:${mobilePort}/#pair=${encodeURIComponent(ticket)}`,
    );

    // Wait for the authenticated view BEFORE checking the URL.
    // Connect.tsx strips `#pair=` via `replaceState` once login
    // completes — same transition the UI assertion guards.
    // Asserting the URL first would race the strip (issue #500).
    await expect(
      page.getByTestId('node-list').or(page.getByTestId('nodelist-loading')),
    ).toBeVisible({ timeout: 10000 });
    await expect(page.getByTestId('connect-screen')).toBeHidden();

    // After the login transition the URL must be the bare origin —
    // Connect.tsx:27-33 strips `#pair=` and rewrites to `<path>`. The
    // web-first `toHaveURL` retries automatically against async
    // `history` mutations; a sync `expect(page.url()).toMatch` would
    // race the same transition.
    await expect(page).toHaveURL(`http://127.0.0.1:${mobilePort}/`);

    // bm_session cookie: HttpOnly + SameSite=Strict + Path=/, holds a
    // fresh per-device credential, distinct from the one-use invitation.
    const cookies = await context.cookies(`http://127.0.0.1:${mobilePort}`);
    const session = cookies.find((c) => c.name === 'bm_session');
    expect(session, 'bm_session cookie set').toBeDefined();
    expect(session!.value, 'cookie must NOT hold the pairing invitation').not.toBe(ticket);
    expect(session!.value, 'cookie must be a 32-char lowercase hex token').toMatch(
      /^[0-9a-f]{32}$/,
    );
    expect(session!.httpOnly, 'cookie HttpOnly').toBe(true);
    expect(session!.sameSite, 'cookie SameSite=Strict').toBe('Strict');
    expect(session!.path, 'cookie Path=/').toBe('/');
  });

  test('reload without #pair= reuses bm_session and lands on NodeList', async ({
    page,
  }) => {
    const { ticket } = await invokeViaHttp<{ ticket: string }>('create_pairing_ticket');
    await page.goto(
      `http://127.0.0.1:${mobilePort}/#pair=${encodeURIComponent(ticket)}`,
    );
    await expect(
      page.getByTestId('node-list').or(page.getByTestId('nodelist-loading')),
    ).toBeVisible({ timeout: 10000 });

    // Reload without `#pair=`; the cookie alone should re-auth.
    await page.goto(`http://127.0.0.1:${mobilePort}/`);
    await expect(
      page.getByTestId('node-list').or(page.getByTestId('nodelist-loading')),
    ).toBeVisible({ timeout: 10000 });
    await expect(page.getByTestId('connect-screen')).toBeHidden();
    // URL stays at `/` on reload — the cookie handles auth.
    await expect(page).toHaveURL(`http://127.0.0.1:${mobilePort}/`);
  });

  test('cold load (no #pair=, no cookie) renders Connect screen', async ({
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
