#!/usr/bin/env node
import assert from "node:assert/strict";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
assert.equal(process.env.TREER_WORKLOAD_CREDENTIAL, "sealed-message-fixture");
assert.equal(process.env.TREER_AGENT_ID, "ag_message_fixture");
const root = process.env.FAKE_MESSAGE_ROOT;
const args = process.argv.slice(2);
assert.equal(args[0], "message");
if (args[1] === "receive") {
  assert.deepEqual(args.slice(2), ["--wait", "30000", "--limit", "10"]);
  const delivered = existsSync(join(root, "acks.jsonl"));
  console.log(JSON.stringify({ deliveries: delivered ? [] : [{
    delivery_id: "del_fixture", recipient: { kind: "agent", id: "ag_message_fixture" },
    message: { message_id: "msg_fixture", sender: { kind: "human", id: "user_fixture", name: "Fixture human", path: "/humans/fixture" }, body: "Fixture message", created_at: "2026-10-12T12:00:00Z" },
  }] }));
} else if (args[1] === "ack") {
  assert.deepEqual(args.slice(2), ["del_fixture", "--operation-id", "launcher-message-ack/del_fixture"]);
  const journal = JSON.parse(readFileSync(join(root, ".treer/agents/ag_message_fixture/queue.json"), "utf8"));
  assert.ok(journal.records.some((record) => record.sourceMessage?.messageId === "msg_fixture" && record.sourceMessage.ackPending));
  appendFileSync(join(root, "acks.jsonl"), JSON.stringify(args) + "\n");
  console.log("{}");
} else throw new Error("Unexpected CLI call");
