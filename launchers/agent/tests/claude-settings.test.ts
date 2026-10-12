import assert from "node:assert/strict";
import { mkdtempSync, realpathSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { ClaudeRuntime } from "../src/harness/claude.js";
const temp = () => realpathSync(mkdtempSync(join(tmpdir(), "claude-settings-test-")));
const fake = join(import.meta.dirname, "fake-claude.mjs");
const create = (cwd: string) => new ClaudeRuntime(process.execPath, cwd, { args: [fake] });
async function idle(runtime: ClaudeRuntime) {
  const deadline = Date.now() + 5000;
  while (runtime.snapshot().thread?.status === "running" && Date.now() < deadline) await new Promise((done) => setTimeout(done, 10));
  assert.equal(runtime.snapshot().thread?.status, "idle");
}

test("Claude resolves aliases to actual models, deduplicates Default and applies only supported effort", async () => {
  process.env.FAKE_CLAUDE_RESOLVED_MODELS = "1";
  const runtime = create(temp());
  try {
    await runtime.start();
    assert.equal(runtime.snapshot().thread!.model, "claude-sonnet-test");
    assert.equal(runtime.snapshot().models.filter((model) => model.model === "claude-sonnet-test").length, 1);
    assert.deepEqual(runtime.snapshot().models[0].supportedReasoningEfforts.map((e) => e.reasoningEffort), ["low", "medium", "high"]);
    await runtime.prompt("work"); await idle(runtime);
    assert.ok(runtime.snapshot().models.some((model) => model.model === runtime.snapshot().thread!.model));
    await runtime.updateSettings({ model: "opus", reasoningEffort: "high" });
    assert.equal(runtime.snapshot().thread!.model, "claude-opus-test");
    assert.equal(runtime.snapshot().thread!.reasoningEffort, "high");
    await runtime.updateSettings({ reasoningEffort: null });
    assert.equal(runtime.snapshot().thread!.reasoningEffort, null);
    await assert.rejects(runtime.updateSettings({ reasoningEffort: "ultra" }), /not supported/);
    await runtime.updateSettings({ model: "legacy" });
    assert.equal(runtime.snapshot().thread!.model, "claude-legacy-test");
    assert.deepEqual(runtime.snapshot().models.find((model) => model.id === "legacy")!.supportedReasoningEfforts, []);
    await assert.rejects(runtime.updateSettings({ reasoningEffort: "high" }), /not supported/);
    await assert.rejects(runtime.updateSettings({ model: "invalid" }), /Invalid model/);
    assert.equal(runtime.snapshot().thread!.model, "claude-legacy-test");
  } finally { await runtime.stop(); delete process.env.FAKE_CLAUDE_RESOLVED_MODELS; }
});

test("Claude retains prompts during serialized model changes, including rejected changes", async () => {
  const cwd = temp(); const log = join(cwd, "calls");
  process.env.FAKE_CLAUDE_CALLS = log; process.env.FAKE_CLAUDE_SLOW_SETTINGS = "1";
  const runtime = create(cwd);
  try {
    await runtime.start();
    const id = runtime.snapshot().thread!.id;
    const switching = runtime.updateSettings({ model: "opus" });
    const prompt = runtime.prompt("during switch");
    await Promise.all([switching, prompt]); await idle(runtime);
    assert.equal(runtime.snapshot().thread!.model, "opus");
    const rejected = runtime.updateSettings({ model: "invalid" });
    const retained = runtime.prompt("after rejection");
    await assert.rejects(rejected, /Invalid model/); await retained; await idle(runtime);
    assert.equal(runtime.snapshot().thread!.model, "opus");
    assert.equal(runtime.snapshot().thread!.id, id);
    const calls = readFileSync(log, "utf8").trim().split("\n").map((line) => JSON.parse(line));
    assert.deepEqual(calls.filter((call) => call.type === "user").map((call) => call.message.content[0].text), ["during switch", "after rejection"]);
    assert.equal(calls.filter((call) => call.request?.subtype === "initialize").length, 1);
    assert.ok(!calls.some((call) => call.request?.subtype === "interrupt"));
  } finally { await runtime.stop(); delete process.env.FAKE_CLAUDE_CALLS; delete process.env.FAKE_CLAUDE_SLOW_SETTINGS; }
});

test("Claude older control protocols hide effort, and missing catalogs preserve the actual model", async () => {
  process.env.FAKE_CLAUDE_RESOLVED_MODELS = "1"; process.env.FAKE_CLAUDE_NO_EFFORT = "1";
  const older = create(temp());
  try {
    await older.start();
    assert.ok(older.snapshot().models.every((model) => !model.supportedReasoningEfforts.length));
    await assert.rejects(older.updateSettings({ reasoningEffort: "high" }), /not supported/);
  } finally { await older.stop(); delete process.env.FAKE_CLAUDE_NO_EFFORT; }
  process.env.FAKE_CLAUDE_NO_MODELS = "1";
  const noCatalog = create(temp());
  try {
    await noCatalog.start(); await noCatalog.prompt("work"); await idle(noCatalog);
    assert.equal(noCatalog.snapshot().thread!.model, "claude-sonnet-test");
    assert.ok(noCatalog.snapshot().models.some((model) => model.model === "claude-sonnet-test"));
  } finally { await noCatalog.stop(); delete process.env.FAKE_CLAUDE_NO_MODELS; delete process.env.FAKE_CLAUDE_RESOLVED_MODELS; }
});
