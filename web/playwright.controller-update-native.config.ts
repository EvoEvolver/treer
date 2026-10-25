import { defineConfig } from '@playwright/test'

// Core owns the server and its fresh PostgreSQL/native fixture. Never start Vite.
export default defineConfig({
  testDir: './e2e',
  testMatch: 'controller-update-native.spec.ts',
  workers: 1,
  retries: 0,
  timeout: 660_000,
  reporter: [['line'], ['./e2e/controller-update-diagnostics.mjs']],
  outputDir: '.treer/controller-update-native',
  use: { browserName: 'chromium', headless: true, viewport: { width: 1280, height: 900 }, trace: 'off', screenshot: 'off', video: 'off' },
})
