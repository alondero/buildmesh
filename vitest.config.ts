import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  test: {
    // The forks pool has reported zero-test success in Windows worktrees (#1257).
    pool: 'threads',
    passWithNoTests: false,
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./tests/setup/vitest.setup.ts'],
    include: ['tests/unit/**/*.test.ts', 'tests/unit/**/*.test.tsx', 'tests/integration/**/*.test.ts', 'tests/integration/**/*.test.tsx'],
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
