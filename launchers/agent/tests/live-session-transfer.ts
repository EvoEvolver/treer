// Opt-in native-provider smoke test. Only creates new disposable sessions.
import assert from "node:assert/strict";
import { mkdtemp, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { CodexRuntime } from "../src/harness/codex.js";
import { ClaudeRuntime } from "../src/harness/claude.js";
import type { HarnessRuntime } from "../src/runtime.js";

// Do not inherit a managed Agent's identity or persisted UI selectors.
for (const key of Object.keys(process.env)) if (/^(TREER_|AGENT_UI_|CODEX_UI_)/.test(key)) delete process.env[key];
async function idle(runtime: HarnessRuntime) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    const thread = runtime.snapshot().thread;
    if (thread?.status === "error") throw new Error(thread.lastError ?? "Native provider failed");
    if (thread?.status === "idle" && thread.turns.length) return;
    await new Promise((done) => setTimeout(done, 50));
  }
  throw new Error("Native session did not become idle");
}
for (const harness of ["codex", "claude"] as const) {
  const root = await realpath(await mkdtemp(join(tmpdir(), `treer-transfer-${harness}-`)));
  const command = harness === "codex" ? process.env.CODEX_BIN || "codex" : process.env.CLAUDE_BIN || "claude";
  const create = (cwd: string, id?: string): HarnessRuntime => harness === "codex"
    ? new CodexRuntime(command, cwd, { threadId: id }) : new ClaudeRuntime(command, cwd, { sessionId: id });
  const source = create(root);
  let restored: HarnessRuntime | undefined;
  const environment = { CODEX_HOME: process.env.CODEX_HOME, CLAUDE_CONFIG_DIR: process.env.CLAUDE_CONFIG_DIR };
  try {
    await source.start();
    await source.prompt("Reply exactly SESSION_TRANSFER_SEEN. Do not use tools or read files.");
    await idle(source);
    const bundle = await source.exportSession!();
    await source.stop();
    const imported = await source.importSession!(bundle, { directory: join(root, "restored") });
    Object.assign(process.env, imported.environment);
    restored = create(imported.cwd, imported.sessionId);
    await restored.start();
    assert.equal(restored.snapshot().thread!.id, imported.sessionId);
    assert.notEqual(imported.sessionId, bundle.sessionId);
    assert.ok(restored.snapshot().thread!.turns.some((turn) => turn.items.some((item) => item.text.includes("SESSION_TRANSFER_SEEN"))), "Native resume lost history");
    await restored.prompt("Reply exactly IMPORT_RESUME_OK. Do not use tools or read files.");
    await idle(restored);
    assert.ok(restored.snapshot().thread!.turns.at(-1)!.items.some((item) => item.text.includes("IMPORT_RESUME_OK")));
    console.log(JSON.stringify({ harness, result: "passed", root, source: bundle.sessionId, restored: imported.sessionId }));
  } finally {
    await source.stop();
    await restored?.stop();
    for (const [key, value] of Object.entries(environment)) {
      if (value === undefined) delete process.env[key]; else process.env[key] = value;
    }
  }
}
