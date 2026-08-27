/**
 * Shared Playwright config — the single source of truth for "how slow is
 * slow" (timeout / expect timeout), testDir, and the dev-server URL.
 *
 * Both `playwright.config.ts` (multi-project, webServer-managed) and
 * `playwright.config.standalone.ts` (single chromium project, tests spawn
 * their own buildmesh.exe) spread this base so the two configs can never
 * silently drift on a slow-machine spec (issue #1261: a spec tuned to
 * standalone's 60s / 15s would flake under the default's 30s / 10s and
 * vice versa).
 *
 * The `timeout` / `expect.timeout` values here are the lenient end of the
 * range (matches the original standalone values). Individual specs that
 * need a faster ceiling can still set their own `test.setTimeout(...)`.
 */
import type { PlaywrightTestConfig } from '@playwright/test';

export const baseConfig = {
  testDir: './tests/e2e',
  timeout: 60000,
  expect: {
    timeout: 15000,
  },
  use: {
    baseURL: 'http://localhost:1420',
    trace: 'on-first-retry' as const,
  },
} satisfies PlaywrightTestConfig;
