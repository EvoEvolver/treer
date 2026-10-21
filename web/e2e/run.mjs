// Browser/Vite tests cannot inherit Agent identity, proxies or credentials.
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir, homedir, platform } from "node:os"
import { join } from "node:path"
import { spawnSync } from "node:child_process"
const home = mkdtempSync(join(tmpdir(), "treer-web-tests-"))
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) =>
  /^(PATH|SystemRoot|WINDIR|TMPDIR|TEMP|TMP|LANG|LC_ALL|CI|TERM|PW_PORT|METRICS_SOURCE_CAPTURE)$/.test(key)))
env.HOME = home
env.PLAYWRIGHT_BROWSERS_PATH = process.env.PLAYWRIGHT_BROWSERS_PATH || (
  platform() === "darwin" ? join(homedir(), "Library/Caches/ms-playwright") : join(homedir(), ".cache/ms-playwright"))
try {
  const result = spawnSync(process.execPath, ["node_modules/@playwright/test/cli.js", "test", ...process.argv.slice(2)], { env, stdio: "inherit" })
  if (result.error) throw result.error
  process.exitCode = result.status ?? 1
} finally { rmSync(home, { recursive: true, force: true }) }
