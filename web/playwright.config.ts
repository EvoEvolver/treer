import { defineConfig } from "@playwright/test"

const PORT = Number(process.env.PW_PORT ?? 4173)
const WEB_SERVER_URL = `http://127.0.0.1:${PORT}`

export default defineConfig({
  testDir: "./e2e",
  timeout: 20_000,
  use: {
    baseURL: WEB_SERVER_URL,
    headless: true,
  },
  webServer: {
    command: `pnpm exec vite --port=${PORT} --strict-port --host 127.0.0.1`,
    url: WEB_SERVER_URL,
    reuseExistingServer: false,
    timeout: 60_000,
  },
})
