import assert from "node:assert/strict";
import test from "node:test";

import { createAisServer } from "./server.mjs";

test("serves turn pages and deduplicates prompt operations", async () => {
  const entries = [
    { kind: "message", type: "message", role: "user", id: "u1", content: "first" },
    { kind: "message", type: "message", role: "assistant", id: "a1", content: "ok" },
    { kind: "message", type: "message", role: "user", id: "u2", content: "second" },
  ];
  const prompts = [];
  const ais = createAisServer({
    instanceId: "kit-test",
    capabilities: ["prompt.submit", "transcript.read", "state.observe", "abort"],
    getStatus: async () => ({ busy: prompts.length === 1 && prompts[0] === "busy", status: "idle" }),
    getEntries: async () => entries,
    submitPrompt: async ({ text }) => { prompts.push(text); },
    abort: async () => { prompts.push("abort"); },
  });
  const port = await ais.listen(0);
  const base = `http://127.0.0.1:${port}`;
  try {
    const manifest = await (await fetch(`${base}/v1/manifest`)).json();
    assert.equal(manifest.protocol, "treer.agent-interface/v1");
    const page = await (await fetch(`${base}/v1/transcript`)).json();
    assert.equal(page.page, 0);
    assert.equal(page.page_count, 2);
    assert.deepEqual(page.entries.map((entry) => entry.id), ["u1", "a1"]);
    const request = {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ operation_id: "op-1", text: "hello" }),
    };
    assert.equal((await fetch(`${base}/v1/prompts`, request)).status, 202);
    assert.equal((await fetch(`${base}/v1/prompts`, request)).status, 202);
    assert.deepEqual(prompts, ["hello"]);
    assert.equal((await fetch(`${base}/v1/abort`, { method: "POST" })).status, 202);
    assert.equal(prompts.at(-1), "abort");
  } finally {
    await ais.close();
  }
});
