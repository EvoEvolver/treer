#!/usr/bin/env node
// Closed launcher regression: no enrolled Treer services, inference or credentials.
import { spawn } from "node:child_process";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const argv = process.argv.slice(2);
let launcher = join(root, "launchers/agent");
let selected = "codex,acp,claude";
for (let index = 0; index < argv.length; index++) {
  if (argv[index] === "--launcher-dir" && argv[index + 1]) launcher = resolve(argv[++index]);
  else if (argv[index] === "--harness" && argv[index + 1]) selected = argv[++index];
  else throw new Error("usage: node scripts/ais-e2e.mjs [--harness codex,acp,claude] [--launcher-dir PATH]");
}
const suites = {
  codex: ["resume.test.ts", "ux.test.ts", "queue.test.ts", "goals.test.ts"],
  acp: ["acp.test.ts"],
  claude: ["claude.test.ts", "claude-http.test.ts", "claude-settings.test.ts"],
};
const harnesses = [...new Set(selected.split(","))];
if (!harnesses.length || harnesses.some((name) => !Object.hasOwn(suites, name))) {
  throw new Error("harness must be codex, acp or claude (comma-separated)");
}
const tests = [...new Set(harnesses.flatMap((name) => suites[name]))].map((name) => join(launcher, "tests", name));
const runner = join(launcher, "node_modules/tsx/dist/cli.mjs");
await Promise.all([runner, ...tests].map((path) => stat(path)));
const { hermeticEnv } = await import(pathToFileURL(join(launcher, "tests/hermetic.mjs")));
const home = await mkdtemp(join(tmpdir(), "treer-launcher-regression-"));
try {
  const env = hermeticEnv();
  // Remove provider credentials/proxies as well as the managed identity scrubbed
  // by the launcher's shared helper. Isolated HOME prevents saved provider auth.
  for (const key of Object.keys(env)) {
    if (/(?:TOKEN|SECRET|PASSWORD|CREDENTIAL|AUTHORIZATION|API_KEY|_PROXY)$/i.test(key) ||
        /^(?:CODEX_HOME|CLAUDE_CONFIG_DIR|FAKE_|XDG_)/.test(key)) delete env[key];
  }
  Object.assign(env, { HOME: home, CODEX_HOME: join(home, ".codex"),
    CLAUDE_CONFIG_DIR: join(home, ".claude"), NO_PROXY: "127.0.0.1,localhost,::1",
    CODEX_BIN: join(launcher, "tests/fake-codex.mjs"),
    CLAUDE_BIN: join(launcher, "tests/fake-claude.mjs") });
  console.log(`Closed Agent Interface regression: ${harnesses.join(", ")}`);
  const child = spawn(process.execPath, [runner, "--test", ...tests], {
    cwd: launcher, env, stdio: ["ignore", "inherit", "inherit"],
  });
  const code = await new Promise((accept, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => signal ? reject(new Error(`test runner terminated: ${signal}`)) : accept(code));
  });
  process.exitCode = code ?? 1;
} finally {
  await rm(home, { recursive: true, force: true });
}
