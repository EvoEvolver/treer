import { EventEmitter } from "node:events";
import assert from "node:assert/strict";
import test from "node:test";

import { createAcpBackend } from "../ais-kit/acp.mjs";
import { cursorAcpArgs, startCursorAis } from "./sidecar.mjs";

test("Cursor AIS launches cursor-agent ACP, not grok's agent alias", () => {
  assert.deepEqual(
    cursorAcpArgs({}),
    ["--trust", "--force", "--sandbox", "disabled", "acp"],
  );
  assert.equal(cursorAcpArgs({})[cursorAcpArgs({}).length - 1], "acp");
});

test("Cursor AIS pages ACP turns", async () => {
  const events = new EventEmitter();
  const rpc = {
    events,
    request: async (method, params) => {
      if (method === "initialize") {
        return { protocolVersion: 1, authMethods: [{ id: "cursor_login" }] };
      }
      if (method === "authenticate") {
        assert.equal(params.methodId, "cursor_login");
        return {};
      }
      if (method === "session/new") return { sessionId: "ses_cursor" };
      if (method === "session/prompt") {
        events.emit("session/update", {
          sessionId: "ses_cursor",
          update: {
            sessionUpdate: "agent_message_chunk",
            content: { type: "text", text: "ack" },
          },
        });
        return { stopReason: "end_turn" };
      }
      throw new Error(`unexpected ${method}`);
    },
    notify() {},
    respond() {},
    close() {},
  };
  const previous = process.env.AIS_AUTO_REGISTER;
  process.env.AIS_AUTO_REGISTER = "0";
  delete process.env.TREER_AGENT_ID;
  const runtime = await startCursorAis({
    rpc,
    backend: createAcpBackend(rpc, {
      cwd: "/tmp",
      selectAuth: (methods) => methods[0]?.id ?? null,
    }),
    child: { kill() {} },
  });
  try {
    await fetch(`http://127.0.0.1:${runtime.port}/v1/prompts`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ operation_id: "op-1", text: "first" }),
    });
    await fetch(`http://127.0.0.1:${runtime.port}/v1/prompts`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ operation_id: "op-2", text: "second" }),
    });
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
