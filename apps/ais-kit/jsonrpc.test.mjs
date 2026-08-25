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
