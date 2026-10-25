import { defineConfig } from '@playwright/test'
export default defineConfig({ testDir: './e2e', testMatch: 'controller-update-observer.spec.ts', workers: 1, retries: 0, timeout: 15000, outputDir: '../.treer/reviews/controller-update-observer-browser', use: { browserName: 'chromium', headless: true, trace: 'off', screenshot: 'off', video: 'off' } })
