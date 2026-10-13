#!/usr/bin/env node
// Controller validate_interface_manifest parity: compare normalized capability
// sets, protocol, instance and UI path against the actual live manifest.
import assert from "node:assert/strict";
import { appendFileSync } from "node:fs";
import { get } from "node:http";
const args = process.argv.slice(2);
if (args.join(" ") === "interface clear") process.exit(0);
assert.deepEqual(args.slice(0, 2), ["interface", "register"]);
const values = (flag) => args.flatMap((arg, index) => arg === flag ? [args[index + 1]] : []);
const port = values("--port")[0];
const manifest = await new Promise((resolve, reject) => {
  get(`http://127.0.0.1:${port}/v1/manifest`, {
    headers: { host: "agent-interface", "x-treer-interface-source": "controller-api", "x-treer-agent-id": process.env.TREER_AGENT_ID, "x-treer-interface-instance": values("--instance-id")[0] },
  }, (response) => {
    assert.equal(response.statusCode, 200);
    let body = "";
    response.on("data", (chunk) => { body += chunk; });
    response.on("end", () => { try { resolve(JSON.parse(body)); } catch (error) { reject(error); } });
  }).on("error", reject);
});
assert.equal(manifest.protocol, "treer.agent-interface/v1");
assert.equal(manifest.instance_id, values("--instance-id")[0]);
assert.equal(manifest.ui_path ?? null, values("--ui-path")[0] ?? null);
assert.deepEqual([...new Set(manifest.capabilities)].sort(), [...new Set(values("--capability"))].sort());
appendFileSync(process.env.FAKE_REGISTRATION_LOG, JSON.stringify({ args, manifest, port: Number(port) }) + "\n");
console.log(JSON.stringify({ registered: true }));
