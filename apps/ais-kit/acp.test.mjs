import { EventEmitter } from "node:events";
import assert from "node:assert/strict";
import test from "node:test";

import { createAcpBackend, selectAuthMethod } from "./acp.mjs";

function fakeAcpRpc(handlers = {}) {
  const events = new EventEmitter();
  const rpc = {
    events,
    responses: [],
    request: async (method, params) => {
      if (handlers[method]) return handlers[method](params);
      if (method === "initialize") return { protocolVersion: 1, authMethods: [] };
      if (method === "session/new") return { sessionId: "ses_1" };
      if (method === "session/prompt") {
        events.emit("session/update", {
          sessionId: "ses_1",
          update: {
            sessionUpdate: "user_message_chunk",
            content: { type: "text", text: params.prompt[0].text },
          },
        });
        events.emit("session/update", {
          sessionId: "ses_1",
          update: {
            sessionUpdate: "agent_thought_chunk",
            content: { type: "text", text: "thinking" },
          },
        });
        for (const chunk of ["PING", "-", "1"]) {
          events.emit("session/update", {
            sessionId: "ses_1",
            update: {
              sessionUpdate: "agent_message_chunk",
              content: { type: "text", text: chunk },
            },
          });
        }
        return { stopReason: "end_turn" };
      }
      if (method === "session/cancel") return {};
      throw new Error(`unexpected ${method}`);
    },
    notify() {},
    respond(id, result) {
      rpc.responses.push({ id, result });
    },
    close() {},
  };
  return rpc;
}

test("selectAuthMethod prefers a listed id", () => {
  assert.equal(
    selectAuthMethod(
      [{ id: "cached_token" }, { id: "xai.api_key" }],
      ["xai.api_key", "cached_token"],
    ),
    "xai.api_key",
  );
  assert.equal(selectAuthMethod([{ id: "cursor_login" }]), "cursor_login");
});

test("ACP backend keeps one session and concatenates message chunks", async () => {
  const rpc = fakeAcpRpc();
  const backend = createAcpBackend(rpc, { cwd: "/tmp" });
  await backend.start();
  assert.equal(backend.sessionId(), "ses_1");
  await backend.prompt("Reply with exactly PING-1 and nothing else.");
  const entries = await backend.entries();
  assert.equal(entries.length, 2);
  assert.equal(entries[0].role, "user");
  assert.equal(entries[1].role, "assistant");
  assert.equal(entries[1].content, "PING-1");
  assert.equal(entries[1]._streaming, undefined);
});

test("ACP backend auto-approves permission requests", async () => {
  const rpc = fakeAcpRpc();
  const backend = createAcpBackend(rpc, { cwd: "/tmp" });
  await backend.start();
  rpc.events.emit("request", "session/request_permission", {
    options: [
      { optionId: "reject-once", kind: "reject_once" },
      { optionId: "allow-once", kind: "allow_once" },
    ],
  }, 9);
  assert.deepEqual(rpc.responses[0], {
    id: 9,
    result: { outcome: { outcome: "selected", optionId: "allow-once" } },
  });
  rpc.events.emit("request", "cursor/ask_question", { questions: [] }, 10);
  assert.equal(rpc.responses[1].result.outcome.outcome, "skipped");
});
