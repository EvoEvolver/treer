import { EventEmitter } from "node:events";
import assert from "node:assert/strict";
import test from "node:test";

import { createDshHostBackend, createDshSdkBackend } from "./backend.mjs";
import { startDshAis } from "./sidecar.mjs";

test("DSH host backend creates one session and maps history events", async () => {
  const created = [];
  const http = async (path, init) => {
    const body = JSON.parse(init.body);
    if (path === "/api/session.create") {
      created.push(body.payload);
      return {
        ok: true,
        json: async () => ({ result: { ok: true, value: { sessionId: "ses_dsh" } } }),
      };
    }
    if (path === "/api/session.history") {
      assert.equal(body.payload.sessionId, "ses_dsh");
      return {
        ok: true,
        json: async () => ({
          result: {
            ok: true,
            value: {
              events: [
                { event: { type: "user/message", seq: 1, data: { text: "first" } } },
                { event: { type: "assistant/message", seq: 2, data: { text: "ok" } } },
                { event: { type: "user/message", seq: 3, data: { text: "second" } } },
              ],
            },
          },
        }),
      };
    }
    if (path === "/api/session.prompt") {
      assert.equal(body.payload.sessionId, "ses_dsh");
      return { ok: true, json: async () => ({ result: { ok: true, value: { accepted: true } } }) };
    }
    if (path === "/api/session.cancel") {
      return { ok: true, json: async () => ({ result: { ok: true, value: { accepted: true } } }) };
    }
    return { ok: false, status: 404, text: async () => "missing" };
  };
  const backend = createDshHostBackend(http, { cwd: "/tmp" });
  await backend.start();
  assert.equal(backend.sessionId(), "ses_dsh");
  assert.equal(created.length, 1);
  const entries = await backend.entries();
  assert.equal(entries[0].role, "user");
  await backend.prompt("hello");
  await backend.abort();
});

test("DSH host backend filters system reminders and uses turn events for busy", async () => {
  const http = async (path) => {
    if (path === "/api/session.create") {
      return { ok: true, json: async () => ({ result: { ok: true, value: { sessionId: "ses_dsh" } } }) };
    }
    if (path === "/api/session.history") {
      return {
        ok: true,
        json: async () => ({
          result: {
            ok: true,
            value: {
              events: [
                { event: { type: "turn/start", seq: 1 } },
                { event: { type: "user/message", seq: 2, data: { content: [{ type: "text", text: "first" }] } } },
                { event: { type: "user/message", seq: 3, data: { content: [{ type: "text", text: "<system-reminder>\nignore" }] } } },
                { event: { type: "assistant/chunk", seq: 4, data: { text: "x" } } },
                { event: { type: "assistant/message", seq: 5, data: { content: [{ type: "text", text: "ok" }] } } },
                { event: { type: "turn/end", seq: 6 } },
              ],
            },
          },
        }),
      };
    }
    return { ok: true, json: async () => ({ result: { ok: true, value: {} } }) };
  };
  const backend = createDshHostBackend(http, { cwd: "/tmp" });
  await backend.start();
  const status = await backend.status();
  assert.equal(status.busy, false);
  const entries = await backend.entries();
  assert.deepEqual(entries.map((entry) => entry.role), ["user", "assistant"]);
  assert.equal(entries[0].content, "first");
});

test("DSH SDK backend uses one session id", async () => {
  const rpc = {
    events: new EventEmitter(),
    request: async (method, params) => {
      if (method === "initialize") return { serverInfo: { name: "deepseek-harness-sdk-runtime" } };
      if (method === "session/prompt") {
        rpc.events.emit("session.event", {
          sessionId: params.sessionId,
          event: { type: "user/message", seq: 1, data: params.contentBlocks },
        });
        return { messageId: "m1" };
      }
      throw new Error(method);
    },
  };
  const backend = createDshSdkBackend(rpc, { sessionId: "fixed" });
  await backend.start();
  await backend.prompt("hello");
  const entries = await backend.entries();
  assert.equal(backend.sessionId(), "fixed");
  assert.equal(entries[0].role, "user");
});

test("DSH AIS pages host history by turn", async () => {
  const http = async (path, init) => {
    const body = JSON.parse(init.body);
    if (path === "/api/session.create") {
      return { ok: true, json: async () => ({ result: { ok: true, value: { sessionId: "ses_dsh" } } }) };
    }
    if (path === "/api/session.history") {
      return {
        ok: true,
        json: async () => ({
          result: {
            ok: true,
            value: {
              events: [
                { event: { type: "user/message", seq: 1, data: "first" } },
                { event: { type: "assistant/message", seq: 2, data: "ok" } },
                { event: { type: "user/message", seq: 3, data: "second" } },
              ],
            },
          },
        }),
      };
    }
    return { ok: true, json: async () => ({ result: { ok: true, value: {} } }) };
  };
  const previous = process.env.AIS_AUTO_REGISTER;
  process.env.AIS_AUTO_REGISTER = "0";
  delete process.env.TREER_AGENT_ID;
  const runtime = await startDshAis({
    backend: createDshHostBackend(http, { cwd: "/tmp" }),
  });
  try {
    const page = await (await fetch(`http://127.0.0.1:${runtime.port}/v1/transcript`)).json();
    assert.equal(page.page_count, 2);
    assert.equal(page.entries[0].id, "1");
  } finally {
    await runtime.shutdown();
    if (previous === undefined) delete process.env.AIS_AUTO_REGISTER;
    else process.env.AIS_AUTO_REGISTER = previous;
  }
});
