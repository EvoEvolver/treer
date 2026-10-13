#!/usr/bin/env node
import { request } from "node:http";
const [status, ...args] = process.argv.slice(2);
if (!["done", "blocked", "stale"].includes(status)) throw new Error("Usage: node $AGENT_UI_GOAL_COMMAND done|blocked|stale --reason TEXT [--required-action TEXT]");
const input = { status };
for (let index = 0; index < args.length; index += 2) {
  const key = { "--reason": "reason", "--required-action": "requiredAction" }[args[index]];
  if (!key || !args[index + 1]) throw new Error("Expected --reason or --required-action followed by text");
  input[key] = args[index + 1];
}
if (!input.reason) throw new Error("--reason is required");
const url = new URL(process.env.AGENT_UI_URL || "");
if (url.protocol !== "http:" || url.hostname !== "127.0.0.1" || !url.port) throw new Error("Missing local AGENT_UI_URL; restart with the updated launcher");
await new Promise((resolve, reject) => {
  const req = request(new URL("/v1/goal/exit", url), { method: "POST", headers: { "content-type": "application/json" } }, (response) => {
    let body = "";
    response.on("data", (chunk) => { body += chunk; });
    response.on("end", () => {
      if (response.statusCode !== 202) reject(new Error(`Goal exit failed (${response.statusCode}): ${body}`));
      else { console.log(body); resolve(); }
    });
  });
  req.setTimeout(70000, () => req.destroy(new Error("Goal exit acknowledgement timed out")));
  req.on("error", reject); req.end(JSON.stringify(input));
});
