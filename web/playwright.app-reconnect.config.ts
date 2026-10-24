import { defineConfig } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  testMatch: 'app-reconnect.spec.ts',
  outputDir: '../.treer/reviews/app242-browser',
  workers: 1,
  projects: ['chromium', 'firefox', 'webkit'].map(browserName => ({
    name: browserName,
    use: { browserName: browserName as 'chromium' | 'firefox' | 'webkit' },
  })),
  use: { baseURL: 'http://127.0.0.1:19142', headless: true },
  webServer: {
    command: 'node node_modules/vite/bin/vite.js --config vite.app-reconnect.config.ts --port 19142 --strict-port --host 127.0.0.1',
    url: 'http://127.0.0.1:19142',
    reuseExistingServer: false,
  },
})
