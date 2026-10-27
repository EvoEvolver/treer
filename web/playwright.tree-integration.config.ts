import { defineConfig } from '@playwright/test'
import { ports } from './e2e/ports'
const origin = `http://127.0.0.1:${ports.web}`
export default defineConfig({
 testDir: './e2e',
 testMatch: ['tree-hitarea.spec.ts', 'app-owned-tree.spec.ts', 'tree-add-machine.spec.ts', 'tree-compact.spec.ts', 'touch-targets.spec.ts', 'workspace-overview.spec.ts', 'session-transfer-managed.spec.ts', 'controller-update-client.spec.ts'],
 workers: 1, retries: 0, timeout: 25000,
 outputDir: '../.treer/reviews/tree-integration-browser',
 use: { baseURL: origin, headless: true },
 webServer: { command: `node node_modules/vite/bin/vite.js --host 127.0.0.1 --port ${ports.web} --strictPort`, url: origin, reuseExistingServer: false },
})
