import { defineConfig, devices } from '@playwright/test';
import { baseConfig } from './playwright.config.base';

export default defineConfig({
  ...baseConfig,
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  // No webServer - tests spawn their own buildmesh.exe.
});
