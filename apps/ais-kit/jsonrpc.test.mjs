import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import test from "node:test";

import { createJsonRpcClient } from "./jsonrpc.mjs";

test("JSON-RPC error notifications do not crash the process", async () => {
  const stdin = new PassThrough();
  const stdout = new PassThrough();
  const rpc = createJsonRpcClient({ stdin, stdout });
  const seen = [];
  rpc.events.on("notification", (method, params) => seen.push([method, params]));
  rpc.events.on("server-error", (params) => seen.push(["server-error", params]));
  stdout.write(`${JSON.stringify({
    method: "error",
    params: { message: "Reconnecting... 1/5", willRetry: true },
  })}\n`);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(seen, [
    ["error", { message: "Reconnecting... 1/5", willRetry: true }],
    ["server-error", { message: "Reconnecting... 1/5", willRetry: true }],
  ]);
  rpc.close();
});

test("JSON-RPC incoming requests can be answered", async () => {
  const stdin = new PassThrough();
  const stdout = new PassThrough();
  const written = [];
  stdin.write = (chunk) => {
    written.push(String(chunk));
    return true;
  };
  const rpc = createJsonRpcClient({ stdin, stdout, includeJsonrpc: true });
  const seen = [];
  rpc.events.on("request", (method, params, id) => seen.push([method, params, id]));
  stdout.write(`${JSON.stringify({
    jsonrpc: "2.0",
    id: 7,
    method: "session/request_permission",
    params: { options: [{ optionId: "allow-once" }] },
  })}\n`);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(seen, [
    ["session/request_permission", { options: [{ optionId: "allow-once" }] }, 7],
  ]);
  rpc.respond(7, { outcome: { outcome: "selected", optionId: "allow-once" } });
  assert.equal(
    written.at(-1),
    `${JSON.stringify({
      id: 7,
      result: { outcome: { outcome: "selected", optionId: "allow-once" } },
      jsonrpc: "2.0",
    })}\n`,
  );
  rpc.close();
});
