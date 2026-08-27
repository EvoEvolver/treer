import assert from "node:assert/strict";
import test from "node:test";

import { createOpenCodeBackend } from "./backend.mjs";
import { startOpenCodeAis } from "./sidecar.mjs";

test("OpenCode backend binds one session and maps messages", async () => {
  const calls = [];
  const http = async (path, init) => {
    calls.push([init?.method ?? "GET", path, init?.body]);
    if (path === "/session" && init?.method === "POST") {
      return { ok: true, json: async () => ({ id: "ses_1" }) };
    }
    if (path === "/session/ses_1/message") {
      return { ok: true, json: async () => ([
        { info: { id: "u1", role: "user" }, parts: [{ type: "text", text: "first" }] },
        { info: { id: "a1", role: "assistant" }, parts: [{ type: "text", text: "ok" }] },
        { info: { id: "u2", role: "user" }, parts: [{ type: "text", text: "second" }] },
      ]) };
    }
    if (path === "/session/ses_1/prompt_async") {
      return { ok: true, status: 204, json: async () => null };
    }
    if (path === "/session/ses_1/abort") {
      return { ok: true, json: async () => true };
    }
    return { ok: false, status: 404, text: async () => "missing" };
  };
  const backend = createOpenCodeBackend(http);
  await backend.start();
  assert.equal(backend.sessionId(), "ses_1");
  assert.equal(calls.filter((call) => call[0] === "POST" && call[1] === "/session").length, 1);
  const entries = await backend.entries();
  assert.equal(entries[0].role, "user");
  await backend.prompt("hello");
  await backend.abort();
  assert.ok(calls.some((call) => call[1] === "/session/ses_1/prompt_async"));
});

test("OpenCode AIS pages by conversation turn", async () => {
  const http = async (path, init) => {
    if (path === "/session" && init?.method === "POST") {
      return { ok: true, json: async () => ({ id: "ses_1" }) };
    }
    if (path === "/session/ses_1/message") {
      return { ok: true, json: async () => ([
        { info: { id: "u1", role: "user" }, parts: [{ type: "text", text: "first" }] },
        { info: { id: "a1", role: "assistant" }, parts: [{ type: "text", text: "ok" }] },
        { info: { id: "u2", role: "user" }, parts: [{ type: "text", text: "second" }] },
      ]) };
    }
    return { ok: true, json: async () => ({}) };
  };
  const previous = process.env.AIS_AUTO_REGISTER;
  process.env.AIS_AUTO_REGISTER = "0";
  delete process.env.TREER_AGENT_ID;
  const runtime = await startOpenCodeAis({
    baseUrl: "http://127.0.0.1:9",
    http,
  });
  try {
    const page = await (await fetch(`http://127.0.0.1:${runtime.port}/v1/transcript`)).json();
    assert.equal(page.page_count, 2);
    assert.deepEqual(page.entries.map((entry) => entry.id), ["u1", "a1"]);
  } finally {
    await runtime.shutdown();
    if (previous === undefined) delete process.env.AIS_AUTO_REGISTER;
    else process.env.AIS_AUTO_REGISTER = previous;
  }
});
