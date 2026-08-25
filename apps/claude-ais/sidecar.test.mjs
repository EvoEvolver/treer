import assert from "node:assert/strict";
import test from "node:test";

import { createClaudeBackend } from "./backend.mjs";
import { startClaudeAis } from "./sidecar.mjs";

test("Claude backend keeps one stream-json session", async () => {
  const sent = [];
  const listeners = [];
  const io = {
    onEvent(listener) { listeners.push(listener); },
    async send(message) {
      sent.push(message);
      if (message.type === "user") {
        for (const listener of listeners) {
          listener({ type: "user", uuid: "u1", message: message.message });
          listener({
            type: "assistant",
            uuid: "a1",
            message: { content: [{ type: "text", text: "ok" }] },
          });
          listener({ type: "result", is_error: false });
        }
      }
    },
  };
  const backend = createClaudeBackend(io);
  for (const listener of listeners) listener({ type: "system", subtype: "init", session_id: "ses_claude" });
  await backend.prompt("first");
  const entries = await backend.entries();
  assert.equal(backend.sessionId(), "ses_claude");
  assert.equal(entries[0].role, "user");
  assert.equal(entries[1].role, "assistant");
  assert.equal(sent[0].type, "user");
});

test("Claude AIS pages stream-json turns", async () => {
  const listeners = [];
  const io = {
    onEvent(listener) { listeners.push(listener); },
    async send(message) {
      if (message.type !== "user") return;
      const text = message.message.content[0].text;
      for (const listener of listeners) {
        listener({ type: "user", uuid: text, message: message.message });
        listener({
          type: "assistant",
          uuid: `${text}-a`,
          message: { content: [{ type: "text", text: "ack" }] },
        });
      }
    },
  };
  const backend = createClaudeBackend(io);
  await backend.prompt("first");
  await backend.prompt("second");
  const previous = process.env.AIS_AUTO_REGISTER;
  process.env.AIS_AUTO_REGISTER = "0";
  delete process.env.TREER_AGENT_ID;
  const runtime = await startClaudeAis({
    io,
    backend,
    child: { kill() {} },
  });
  try {
    const page = await (await fetch(`http://127.0.0.1:${runtime.port}/v1/transcript`)).json();
    assert.equal(page.page_count, 2);
    assert.equal(page.entries[0].role, "user");
    assert.equal(page.entries.at(-1).role, "assistant");
  } finally {
    await runtime.shutdown();
    if (previous === undefined) delete process.env.AIS_AUTO_REGISTER;
    else process.env.AIS_AUTO_REGISTER = previous;
  }
});
