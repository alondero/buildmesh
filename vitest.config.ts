import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

const testFiles = ['tests/unit/**/*.test.ts', 'tests/unit/**/*.test.tsx', 'tests/integration/**/*.test.ts', 'tests/integration/**/*.test.tsx'];
const browserFiles = [
  'tests/integration/ui-shot.test.ts',
  'tests/integration/ui-shot-errors.test.ts',
  'tests/integration/ui-shot-server.test.ts',
  'tests/integration/tauri-mock-settings.test.ts',
];

export default defineConfig({
  plugins: [react()],
  test: {
    // The forks pool has reported zero-test success in Windows worktrees (#1257).
    pool: 'threads',
    passWithNoTests: false,
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./tests/setup/vitest.setup.ts'],
    projects: [
      { extends: true, test: { name: 'jsdom', include: testFiles, exclude: browserFiles } },
      {
        extends: true,
        test: {
          name: 'browser-tools',
          include: browserFiles,
          // Each file owns real Chromium/Vite processes. Run after the jsdom
          // pool drains, one file at a time, instead of competing with hundreds
          // of component transforms and other browser launches (#2539).
          sequence: { groupOrder: 1 },
          fileParallelism: false,
          // The diagnostic CLI helper has a 60s child deadline and some tests
          // invoke it twice. The owner must outlive the work it supervises.
          testTimeout: 150000,
        },
      },
    ],
    // Vitest's 5s default is too tight for this suite on Windows. With 288
    // files running concurrently, ordinary filesystem tests (mkdtemp/mkdir/
    // writeFile under an antivirus-scanned temp dir) and userEvent interaction
    // tests routinely take 5-6s of wall clock while their assertions pass in
    // milliseconds, so the budget — not the behaviour — decides the result.
    // Tests that shell out or drive a browser still declare their own larger
    // budget where the work genuinely warrants it. 30s stays far below a real
    // hang, so this does not mask one (same reasoning as issue #2049).
    testTimeout: 30000,
    // Runtime errors invalidate the run even when assertions pass (#1452).
    dangerouslyIgnoreUnhandledErrors: false,
    coverage: {
      reporter: ['text', 'json', 'html'],
      include: ['src/**/*.{ts,tsx}'],
      exclude: ['src/**/*.d.ts', 'src/**/index.ts'],
    },
  },
});
