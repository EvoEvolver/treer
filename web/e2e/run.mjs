// Browser/Vite tests cannot inherit Agent identity, proxies or credentials.
import { mkdtempSync, rmSync } from "node:fs"
import { createServer } from "node:net"
import { tmpdir, homedir, platform } from "node:os"
import { join } from "node:path"
import { spawnSync } from "node:child_process"

// Vite, the local Worker and its inspector use PW_PORT, +1 and +2 (see ports.ts).
// Without an explicit PW_PORT pick a random free triple so parallel runs never collide.
const free = port => new Promise(resolve => {
  const server = createServer().once("error", () => resolve(false))
  server.listen(port, "127.0.0.1", () => server.close(() => resolve(true)))
})
async function randomPorts() {
  for (let attempt = 0; attempt < 50; attempt++) {
    const base = 20000 + Math.floor(Math.random() * 40000)
    if ((await free(base)) && (await free(base + 1)) && (await free(base + 2))) return String(base)
  }
  throw new Error("no free port triple for the browser tests; set PW_PORT")
}

const home = mkdtempSync(join(tmpdir(), "treer-web-tests-"))
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) =>
  /^(PATH|SystemRoot|WINDIR|TMPDIR|TEMP|TMP|LANG|LC_ALL|CI|TERM|PW_PORT)$/.test(key)))
env.HOME = home
env.PW_PORT ??= await randomPorts()
env.PLAYWRIGHT_BROWSERS_PATH = process.env.PLAYWRIGHT_BROWSERS_PATH || (
  platform() === "darwin" ? join(homedir(), "Library/Caches/ms-playwright") : join(homedir(), ".cache/ms-playwright"))
try {
  const result = spawnSync(process.execPath, ["node_modules/@playwright/test/cli.js", "test", ...process.argv.slice(2)], { env, stdio: "inherit" })
  if (result.error) throw result.error
  process.exitCode = result.status ?? 1
} finally { rmSync(home, { recursive: true, force: true }) }
