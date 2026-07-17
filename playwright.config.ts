import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './tests/e2e',
  timeout: 30000,
  expect: {
    timeout: 10000,
  },
  use: {
    baseURL: 'http://localhost:1420',
    trace: 'on-first-retry',
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
      // reuseExistingServer: true means "if :1420 is already up, use it"
      // — no Vite auto-start so the spec is a no-op for users with Vite
      // running AND doesn't pay the Rust-compile cost when they don't.
      // Run `npm run dev` (just Vite) before the spec; the spec itself
      // errors out fast if :1420 is unreachable.
      webServer: {
        command: 'npm run dev',
        url: 'http://localhost:1420',
        reuseExistingServer: true,
        timeout: 60000,
        stdout: 'pipe',
        stderr: 'pipe',
      },
    },
  ],
});