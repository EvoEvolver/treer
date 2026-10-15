import { defineConfig } from "@playwright/test"
import { join } from "node:path"
import { homedir } from "node:os"
import { ports, workerOrigin } from "./e2e/ports"

const workerState = "'" + join(homedir(), "worker-security-state").replaceAll("'", "'\\''") + "'"
const PORT = ports.web
const WEB_SERVER_URL = `http://127.0.0.1:${PORT}`

export default defineConfig({
  testDir: "./e2e",
  timeout: 20_000,
  use: {
    baseURL: WEB_SERVER_URL,
    headless: true,
  },
  webServer: [{
    command: `node node_modules/vite/bin/vite.js --port=${PORT} --strict-port --host 127.0.0.1`,
    url: WEB_SERVER_URL,
    reuseExistingServer: false,
    timeout: 60_000,
  }, {
    name: "local security Worker",
    command: `node node_modules/wrangler/bin/wrangler.js dev --local --ip 127.0.0.1 --port ${ports.worker} --persist-to ${workerState} --var TREER_PROXY_PUBLIC_URL:${workerOrigin}`,
    url: `${workerOrigin}/health`,
    reuseExistingServer: false,
    timeout: 60_000,
    env: { WRANGLER_SEND_METRICS: "false", BROWSER: "none" },
  }],
})
