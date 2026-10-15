import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { mkdtemp, mkdir, readFile, realpath, rm, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";
import { completionKey } from "../src/prompt-submission.js";
const launcher = resolve(import.meta.dirname, "..");
async function until<T>(read: () => Promise<T>, matches: (value: T) => boolean): Promise<T> {
  const deadline = Date.now() + 8000;
  while (Date.now() < deadline) {
    try { const value = await read(); if (matches(value)) return value; } catch { /* starting */ }
    await new Promise((done) => setTimeout(done, 20));
  }
  throw new Error("HTTP queue test condition timed out");
}
async function stop(child: ChildProcess) {
  if (child.exitCode !== null) return;
  const done = new Promise((resolve) => child.once("exit", resolve)); child.kill("SIGTERM"); await done;
}
test("AIS queue persists across launcher restart, exposes correlated results and uses only the fixture CLI for notifications", async (t) => {
  const cwd = await realpath(await mkdtemp(join(tmpdir(), "queue-http-")));
  const children: ChildProcess[] = [];
  t.after(async () => {
    // Hooks run in registration order: stop writers before deleting their journal.
    for (const child of children) await stop(child);
    await rm(cwd, { recursive: true, force: true });
  });
  const home = join(cwd, "claude"), bin = join(cwd, "bin"), calls = join(cwd, "notifications");
  await mkdir(home); await mkdir(bin);
  await writeFile(join(bin, "treer"), `#!/usr/bin/env node\nimport fs from 'node:fs';\nlet body='';for await(const chunk of process.stdin)body+=chunk;fs.appendFileSync(process.env.FAKE_NOTIFY_CALLS,JSON.stringify({args:process.argv.slice(2),body,agent:process.env.TREER_AGENT_ID})+'\\n');\n`, { mode: 0o700 });
  const launch = async () => {
    const portFile = join(cwd, "port"); await unlink(portFile).catch(() => {});
    const child = spawn(join(launcher, "node_modules/.bin/tsx"), [join(launcher, "src/index.ts")], { stdio: "pipe", env: hermeticEnv({
      AGENT_UI_HARNESS: "claude", AGENT_UI_CWD: cwd, AGENT_UI_PORT_FILE: portFile, AGENT_UI_PORT: "0", AGENT_UI_INSTANCE_ID: "fixture-instance",
      TREER_AGENT_ID: "ag_fixture", TREER_WORKLOAD_CREDENTIAL: "fixture-only", CLAUDE_BIN: join(launcher, "tests/fake-claude.mjs"), CLAUDE_CONFIG_DIR: home, FAKE_CLAUDE_CONFIG: home,
      PATH: `${bin}:${process.env.PATH}`, FAKE_NOTIFY_CALLS: calls,
    }) });
    child.stdout.resume(); child.stderr.resume();
    children.push(child);
    const base = await until(async () => `http://127.0.0.1:${Number(await readFile(portFile, "utf8"))}`, () => true);
    await until(() => fetch(base + "/api/state").then((response) => response.json()), (state) => state.ready === true);
    const headers = { "content-type": "application/json", host: "agent-interface", "x-treer-interface-source": "controller-api", "x-treer-agent-id": "ag_fixture", "x-treer-interface-instance": "fixture-instance" };
    return { child, base, headers, post: (body: unknown) => fetch(base + "/v1/prompts", { method: "POST", headers, body: JSON.stringify(body) }) };
  };
  const first = await launch();
  assert.equal((await fetch(first.base + "/v1/queue")).status, 403);
  assert.equal((await fetch(first.base + "/v1/queue", { headers: { ...first.headers, "x-treer-interface-instance": "stale" } })).status, 403);
  assert.equal((await fetch(first.base + "/v1/queue", { headers: { ...first.headers, origin: "https://attacker.example" } })).status, 403);
  const hold = await first.post({ operation_id: "hold", text: "hold", mode: "prompt" }).then((response) => response.json());
  const sender = { agent_id: "ag_sender", path: "/team/sender", name: "sender" };
  const body = { operation_id: "queued", text: "continue", sender, delivery: "queue", notify: { on: ["done", "failed", "interrupted"] } };
  const queued = await first.post(body).then((response) => response.json());
  assert.equal(queued.submission.disposition, "queued"); assert.equal(queued.submission.position, 1);
  const status = await fetch(first.base + "/v1/status").then((response) => response.json());
  assert.equal(status.busy, true); assert.equal(status.queue_length, 1);
  const pending = await fetch(first.base + "/v1/queue", { headers: first.headers }).then((response) => response.json());
  assert.equal(pending.submissions[0].sender.path, sender.path);
  await stop(first.child);
  const second = await launch();
  const replay = await second.post(body).then((response) => response.json());
  assert.equal(replay.submission.id, queued.submission.id); assert.equal(replay.duplicate, true);
  const result = await until(() => fetch(second.base + "/v1/submissions/" + queued.submission.id, { headers: second.headers }).then((response) => response.json()), (value) => value.submission?.state === "done");
  assert.ok(result.submission.turn_id); assert.ok(result.submission.assistant_preview);
  const old = await fetch(second.base + "/v1/submissions/" + hold.submission.id, { headers: second.headers }).then((response) => response.json());
  assert.equal(old.submission.state, "interrupted");
  const notified = await until(async () => (await readFile(calls, "utf8")).trim().split("\n").map((line) => JSON.parse(line)), (value) => value.length === 1);
  assert.equal(notified[0].agent, "ag_fixture");
  const journal = JSON.parse(await readFile(join(cwd, ".treer/agents/ag_fixture/queue.json"), "utf8"));
  const record = journal.records.find((record: { submission: { id: string } }) => record.submission.id === queued.submission.id);
  assert.deepEqual(notified[0].args, ["message", "send", "--to", "ag_sender", "--idempotency-key", completionKey(record)]);
  assert.equal(JSON.parse(notified[0].body).status, "done");
  assert.deepEqual(JSON.parse(notified[0].body).submission_ids, [queued.submission.id]);
});
