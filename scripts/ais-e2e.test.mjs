import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { test } from "node:test";

const exec = promisify(execFile);
const script = resolve(import.meta.dirname, "ais-e2e.mjs");
test("closed launcher wrapper removes inherited authority and selects fake suites", async () => {
  const root = await mkdtemp(join(tmpdir(), "treer-regression-wrapper-"));
  try {
    await mkdir(join(root, "tests"));
    await mkdir(join(root, "node_modules/tsx/dist"), { recursive: true });
    await writeFile(join(root, "tests/hermetic.mjs"), `export function hermeticEnv() {
      return Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^(TREER_|AGENT_UI_|CODEX_UI_)/.test(key)));
    }`);
    for (const file of ["resume", "ux", "queue", "goals", "acp", "claude", "claude-http", "claude-settings"])
      await writeFile(join(root, `tests/${file}.test.ts`), "");
    const report = join(root, "report.json");
    await writeFile(join(root, "node_modules/tsx/dist/cli.mjs"), `import {writeFileSync} from 'node:fs';
      writeFileSync(${JSON.stringify(report)}, JSON.stringify({argv:process.argv,env:process.env}));`);
    const inheritedHome = process.env.HOME;
    const result = await exec(process.execPath, [script, "--launcher-dir", root, "--harness", "codex,claude"], {
      env: { ...process.env, TREER_WORKLOAD_CREDENTIAL: "fake-private", AGENT_UI_SESSION_ID: "fake-session",
        OPENAI_API_KEY: "fake-provider", ANTHROPIC_API_KEY: "fake-provider", HTTP_PROXY: "http://127.0.0.1:1" },
    });
    assert.match(result.stdout, /Closed Agent Interface regression/);
    const { argv, env } = JSON.parse(await readFile(report, "utf8"));
    for (const key of ["TREER_WORKLOAD_CREDENTIAL", "AGENT_UI_SESSION_ID", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "HTTP_PROXY"])
      assert.equal(env[key], undefined, key);
    assert.notEqual(env.HOME, inheritedHome);
    assert.ok(env.CODEX_BIN.endsWith("tests/fake-codex.mjs"));
    assert.ok(env.CLAUDE_BIN.endsWith("tests/fake-claude.mjs"));
    assert.ok(argv.includes(join(root, "tests/claude-http.test.ts")));
    assert.ok(!argv.includes(join(root, "tests/acp.test.ts")));
    await assert.rejects(readFile(join(env.HOME, "anything")), { code: "ENOENT" });
    await assert.rejects(exec(process.execPath, [script, "--launcher-dir", root, "--harness", "live"]),
      (error) => /harness must be/.test(error.stderr));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
