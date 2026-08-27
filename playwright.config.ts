import { defineConfig, devices } from '@playwright/test';
import { baseConfig } from './playwright.config.base';

export default defineConfig({
  ...baseConfig,
  // Issue #1261 — auto-start Vite so `npm run test:e2e` works without
  // a manually running dev server. `reuseExistingServer: true` means
  // a user with `npm run dev` (or `npm run tauri dev`) already up
  // pays no extra boot cost; the spec just attaches. Specs that
  // spawn their own buildmesh.exe (mobile-spa) ignore :1420 — they
  // talk to the mobile server on 1992-1994 — so Vite being up is
  // harmless.
  //
  // Declared once at the top level instead of duplicated under every
  // project (Playwright applies top-level `webServer` to all projects
  // — both `chromium` and `verify-smoke` need Vite on :1420).
  //
  // NOTE: `webServer` lives on TestConfig (not TestProject), so a
  // previous version of this config declared it inside `projects[]`
  // and it was silently dropped — Playwright errored at `page.goto('/')`
  // with `ERR_CONNECTION_REFUSED`. Issue #1257 caught and fixed it.
  webServer: {
    command: 'npm run dev',
    url: 'http://localhost:1420',
    reuseExistingServer: true,
    timeout: 60000,
    stdout: 'pipe',
    stderr: 'pipe',
  },
  projects: [
    {
      name: 'chromium',
      testIgnore: /verify-smoke\.spec\.ts$/,
      use: { ...devices['Desktop Chrome'] },
    },
    // Issue #157 — verify-smoke only needs Vite (the Tauri mock from
    // scripts/ui-mock/tauri-mock.mjs replaces the Rust backend, so we
    // skip `npm run tauri dev` and the slow Rust compile). Vite alone
    // serves the React app on :1420; the spec installs the mock IPC
    // via `page.addInitScript` before any app module evaluates.
    {
      name: 'verify-smoke',
      testMatch: /verify-smoke\.spec\.ts$/,
      use: { ...devices['Desktop Chrome'] },
    },
  ],
});
