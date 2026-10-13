import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmodSync, copyFileSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { hermeticEnv } from "./hermetic.mjs";

const root = resolve(import.meta.dirname, "..");
for (const harness of ["codex", "claude"]) {
  test(`${harness} shell startup registers exactly the live manifest with Controller validation`, async () => {
    const cwd = mkdtempSync(join(tmpdir(), "launcher-register-"));
    const log = join(cwd, "registration.jsonl");
    copyFileSync(join(root, "tests/fake-interface-controller.mjs"), join(cwd, "treer"));
    chmodSync(join(cwd, "treer"), 0o700);
    const child = spawn(join(root, "scripts/treer-agent.sh"), ["--harness", harness, "--cwd", cwd], {
      cwd, env: hermeticEnv({ HOME: cwd, PATH: `${cwd}:${process.env.PATH}`, AGENT_UI_PORT: "0", AGENT_UI_MESSAGE_WATCH: "0",
        TREER_AGENT_ID: "ag_registration", AGENT_UI_INSTANCE_ID: `registration-${harness}`,
        CODEX_BIN: join(root, "tests/fake-codex.mjs"), CLAUDE_BIN: join(root, "tests/fake-claude.mjs"), FAKE_REGISTRATION_LOG: log }),
      stdio: "pipe",
    });
    let output = "";
    child.stdout.on("data", (chunk) => { output += chunk; });
    child.stderr.on("data", (chunk) => { output += chunk; });
    try {
      const deadline = Date.now() + 10000;
      while (!output.includes("registered Agent UI") && Date.now() < deadline) {
        if (child.exitCode !== null) throw new Error(`launcher failed: ${output}`);
        await new Promise((resolve) => setTimeout(resolve, 25));
      }
      assert.match(output, /registered Agent UI/, output);
      const registration = JSON.parse(readFileSync(log, "utf8"));
      assert.ok(registration.port > 0);
      assert.equal(registration.manifest.instance_id, `registration-${harness}`);
      // No fixed capability count: any future manifest addition must be registered.
      assert.ok(registration.manifest.capabilities.includes("prompt.queue"));
    } finally {
      if (child.exitCode === null) {
        const exited = new Promise((resolve) => child.once("exit", resolve));
        child.kill("SIGTERM"); await exited;
      }
    }
  });
}
