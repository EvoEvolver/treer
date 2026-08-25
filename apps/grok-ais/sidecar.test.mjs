import { EventEmitter } from "node:events";
import assert from "node:assert/strict";
import test from "node:test";

import { createAcpBackend } from "../ais-kit/acp.mjs";
import { grokAcpArgs, startGrokAis } from "./sidecar.mjs";

test("Grok AIS launches ACP stdio, not the TUI", () => {
  assert.deepEqual(grokAcpArgs({}), ["--no-auto-update", "agent", "--always-approve", "stdio"]);
  assert.deepEqual(
    grokAcpArgs({ GROK_AIS_MODEL: "grok-4.6" }),
    ["--no-auto-update", "agent", "--always-approve", "--model", "grok-4.6", "stdio"],
  );
});

test("Grok AIS pages ACP turns", async () => {
  const events = new EventEmitter();
  const rpc = {
    events,
    request: async (method, params) => {
      if (method === "initialize") {
        return { protocolVersion: 1, authMethods: [{ id: "xai.api_key" }] };
      }
      if (method === "authenticate") {
        assert.equal(params.methodId, "xai.api_key");
        return {};
      }
      if (method === "session/new") return { sessionId: "ses_grok" };
      if (method === "session/prompt") {
        events.emit("session/update", {
          sessionId: "ses_grok",
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
  const previousKey = process.env.XAI_API_KEY;
  process.env.AIS_AUTO_REGISTER = "0";
  process.env.XAI_API_KEY = "test-key";
  delete process.env.TREER_AGENT_ID;
  const runtime = await startGrokAis({
    rpc,
    backend: createAcpBackend(rpc, {
      cwd: "/tmp",
      selectAuth: (methods) => methods[0]?.id ?? null,
      authParams: { _meta: { headless: true } },
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
    if (previousKey === undefined) delete process.env.XAI_API_KEY;
    else process.env.XAI_API_KEY = previousKey;
  }
});
