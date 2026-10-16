import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { AcpRuntime } from "../src/harness/acp.js";
import { ClaudeRuntime } from "../src/harness/claude.js";
import { CodexRuntime } from "../src/harness/codex.js";
import type { HarnessImage } from "../src/runtime.js";
import { mapItem } from "../src/harness/codex-map.js";
import { createHash } from "node:crypto";

const fake = (name: string) => fileURLToPath(new URL(`./fake-${name}.mjs`, import.meta.url));
const image: HarnessImage = { id: `img_${"a".repeat(32)}`, mime: "image/png", size: 3, path: `/private/images/img_${"a".repeat(32)}.png`, data: "YWJj" };
const calls = (file: string) => readFileSync(file, "utf8").trim().split("\n").map(line => JSON.parse(line));
async function waitCall(file: string, predicate: (call: ReturnType<typeof calls>[number]) => boolean) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const call = calls(file).find(predicate);
    if (call) return call;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.fail("Timed out waiting for the fake provider to consume input");
}

test("Claude sends base64 native image blocks and keeps public thumbnail metadata", async t => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "image-claude-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const log = join(root, "calls");
  const runtime = new ClaudeRuntime(process.execPath, root, { args: [fake("claude"), "--calls-file", log], configDir: root });
  try {
    await runtime.start(); await runtime.prompt("look", [image]);
    assert.equal(runtime.harness.capabilities.images, true);
    const input = await waitCall(log, call => call.type === "user");
    assert.deepEqual(input.message.content, [{ type: "text", text: "look" }, { type: "image", source: { type: "base64", media_type: "image/png", data: "YWJj" } }]);
    assert.deepEqual(runtime.snapshot().thread?.turns.at(-1)?.items.find(item => item.kind === "userMessage")?.attachments, [{ id: image.id, mime: image.mime, size: image.size }]);
  } finally { await runtime.stop(); }
});
for (const native of [false, true]) test(`ACP image capability=${native}: native only when advertised`, async t => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "image-acp-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  process.env.FAKE_ACP_CALLS = join(root, "calls"); process.env.FAKE_ACP_IMAGES = native ? "1" : "0";
  const runtime = new AcpRuntime(process.execPath, [fake("acp")], root);
  try {
    await runtime.start(); await runtime.prompt("look", [image]);
    assert.equal(runtime.harness.capabilities.images, native);
    const deadline = Date.now() + 5000;
    while (!calls(join(root, "calls")).some(call => call.method === "session/prompt") && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 10));
    const input = calls(join(root, "calls")).find(call => call.method === "session/prompt");
    assert.deepEqual(input.params.prompt, [{ type: "text", text: "look" }, native ? { type: "image", mimeType: "image/png", data: "YWJj" } : { type: "text", text: `Image attachment (native image input unavailable): ${image.path}` }]);
  } finally { await runtime.stop(); delete process.env.FAKE_ACP_CALLS; delete process.env.FAKE_ACP_IMAGES; }
});
test("Codex uses localImage on start; mapping never exposes private paths", async t => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "image-codex-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  process.env.FAKE_CODEX_CALLS = join(root, "calls");
  const runtime = new CodexRuntime(fake("codex"), root);
  try {
    await runtime.start(); await runtime.prompt("look", [image]);
    const input = calls(join(root, "calls")).find(call => call.method === "turn/start");
    assert.deepEqual(input.params.input, [{ type: "text", text: "look", text_elements: [] }, { type: "localImage", path: image.path }]);
    assert.equal(runtime.harness.capabilities.images, true);
    const mapped = mapItem({ type: "userMessage", content: input.params.input }, "turn", 0);
    assert.deepEqual(mapped.attachments, [{ id: image.id, mime: image.mime, size: 0 }]);
    assert.equal(JSON.stringify(mapped).includes(image.path), false);
  } finally { await runtime.stop(); delete process.env.FAKE_CODEX_CALLS; }
});
test("Codex uses the same localImage blocks when steering a running turn", async t => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "image-codex-steer-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  process.env.FAKE_CODEX_CALLS = join(root, "calls"); process.env.FAKE_CODEX_LIVE = "1";
  const runtime = new CodexRuntime(fake("codex"), root);
  try {
    await runtime.start(); assert.equal(runtime.snapshot().thread?.status, "running");
    await runtime.prompt("new evidence", [image]);
    const steer = calls(join(root, "calls")).find(call => call.method === "turn/steer");
    assert.deepEqual(steer.params.input, [{ type: "text", text: "new evidence", text_elements: [] }, { type: "localImage", path: image.path }]);
  } finally { await runtime.stop(); delete process.env.FAKE_CODEX_CALLS; delete process.env.FAKE_CODEX_LIVE; }
});
for (const mode of ["native", "fallback"]) test(`ACP resumed ${mode} images reconcile without exposing bytes or private paths`, async t => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "image-acp-resume-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  process.env.FAKE_ACP_REPLAY_IMAGE = mode;
  const runtime = new AcpRuntime(process.execPath, [fake("acp")], root, { sessionId: "saved" });
  try {
    await runtime.start();
    const item = runtime.snapshot(1000).thread?.turns.flatMap(turn => turn.items).find(item => item.id === "image-history");
    assert.ok(item);
    if (mode === "native") assert.deepEqual(item.attachmentDigests, [createHash("sha256").update("abc").digest("hex")]);
    else assert.deepEqual(item.attachments?.map(image => image.id), [image.id]);
    assert.equal(JSON.stringify(item).includes("/private/"), false);
    assert.equal(JSON.stringify(item).includes("YWJj"), false);
  } finally { await runtime.stop(); delete process.env.FAKE_ACP_REPLAY_IMAGE; }
});
