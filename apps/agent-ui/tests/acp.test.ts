import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { AcpRuntime } from "../src/harness/acp.js";
import { unifiedDiff } from "../src/harness/diff.js";
import { loadThreadId, threadStatePath } from "../src/thread-state.js";

const fake = fileURLToPath(new URL("./fake-acp.mjs", import.meta.url));
const temp = () => realpathSync(mkdtempSync(join(tmpdir(), "agent-ui-acp-")));
const calls = (file: string) => readFileSync(file, "utf8").trim().split("\n").map((line) => JSON.parse(line) as { method: string; params: Record<string, unknown> });

function runtime(cwd: string, options: ConstructorParameters<typeof AcpRuntime>[3] = {}) {
  return new AcpRuntime(process.execPath, [fake], cwd, options);
}

async function settled(acp: AcpRuntime) {
  const deadline = Date.now() + 5000;
  while (acp.snapshot().thread?.status === "running" && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 10));
}

test("a new ACP session is saved, named by the agent, switched to full access, and exposes its models", async () => {
  const cwd = temp();
  const log = join(cwd, "calls");
  process.env.FAKE_ACP_CALLS = log;
  const stateFile = threadStatePath(cwd, "ag_acp")!;
  const acp = runtime(cwd, { stateFile });
  try {
    await acp.start();
    assert.equal(acp.harness.name, "Fake Agent");
    assert.equal(acp.snapshot().thread?.id, "sess-new");
    assert.equal(loadThreadId(stateFile), "sess-new");
    assert.deepEqual(acp.snapshot().models.map((option) => [option.model, option.isDefault]), [["sonnet", true], ["opus", false]]);
    const modeCall = calls(log).find((call) => call.method === "session/set_config_option");
    assert.deepEqual(modeCall?.params, { sessionId: "sess-new", configId: "mode", value: "bypassPermissions" });
    await acp.updateSettings({ model: "opus" });
    assert.equal(acp.snapshot().thread?.model, "opus");
  } finally { await acp.stop(); delete process.env.FAKE_ACP_CALLS; }
});

test("a turn maps reasoning, plan, commands, approved diffs and streamed replies to shared history", async () => {
  const cwd = temp();
  const log = join(cwd, "calls");
  process.env.FAKE_ACP_CALLS = log;
  const acp = runtime(cwd);
  try {
    await acp.start();
    await acp.prompt("fix it");
    await settled(acp);
    const turn = acp.snapshot().thread!.turns.at(-1)!;
    assert.equal(turn.status, "completed");
    assert.deepEqual(turn.items.map((item) => item.kind), ["userMessage", "reasoning", "plan", "commandExecution", "fileChange", "agentMessage"]);
    const [, reasoning, plan, command, change, reply] = turn.items;
    assert.equal(reasoning.text, "Thinking");
    assert.deepEqual(plan.steps, [{ step: "Inspect", status: "completed" }, { step: "Edit", status: "inProgress" }]);
    assert.equal(command.text, "npm test");
    assert.equal(command.detailText, "13 passed");
    assert.equal(command.status, "completed");
    assert.equal(change.changes?.[0].path, "note.txt");
    assert.match(change.changes![0].diff, /-b\n\+c/);
    assert.equal(reply.text, "Done now.");
    assert.equal(acp.snapshot().thread?.status, "idle");
    const permission = readFileSync(log, "utf8").trim().split("\n").map((line) => JSON.parse(line)).find((entry) => entry.response);
    assert.deepEqual(permission.response, { outcome: { outcome: "selected", optionId: "always" } });
  } finally { await acp.stop(); delete process.env.FAKE_ACP_CALLS; }
});

test("loading a saved session replays history into turns without creating a new session", async () => {
  const cwd = temp();
  const log = join(cwd, "calls");
  process.env.FAKE_ACP_CALLS = log;
  const acp = runtime(cwd, { sessionId: "sess-old" });
  try {
    await acp.start();
    const turns = acp.snapshot().thread!.turns;
    assert.equal(acp.snapshot().thread?.id, "sess-old");
    assert.deepEqual(turns.map((turn) => turn.items.map((item) => [item.kind, item.text])), [
      [["userMessage", "Remember PLUM-42"], ["agentMessage", "noted"]],
      [["userMessage", "list files"], ["commandExecution", "ls"]],
    ]);
    assert.ok(!calls(log).some((call) => call.method === "session/new"));
  } finally { await acp.stop(); delete process.env.FAKE_ACP_CALLS; }
});

test("an unloadable session fails clearly instead of starting a new one", async () => {
  const cwd = temp();
  const log = join(cwd, "calls");
  process.env.FAKE_ACP_CALLS = log;
  const missing = runtime(cwd, { sessionId: "missing" });
  try {
    await assert.rejects(missing.start(), /Failed to load Fake Agent session missing; no new session was created/);
  } finally { await missing.stop(); }
  process.env.FAKE_ACP_NO_LOAD = "1";
  const unsupported = runtime(cwd, { sessionId: "sess-old" });
  try {
    await assert.rejects(unsupported.start(), /cannot load ACP session sess-old/);
    assert.ok(!calls(log).some((call) => call.method === "session/new"));
  } finally { await unsupported.stop(); delete process.env.FAKE_ACP_NO_LOAD; delete process.env.FAKE_ACP_CALLS; }
});

test("interrupt cancels the running turn", async () => {
  process.env.FAKE_ACP_HANG = "1";
  const acp = runtime(temp());
  try {
    await acp.start();
    await acp.prompt("wait");
    assert.equal(acp.snapshot().thread?.status, "running");
    await acp.interrupt();
    await settled(acp);
    assert.equal(acp.snapshot().thread?.turns.at(-1)?.status, "interrupted");
    assert.equal(acp.snapshot().thread?.status, "idle");
  } finally { await acp.stop(); delete process.env.FAKE_ACP_HANG; }
});

test("unified diffs keep context, number hunks, and mark new files", () => {
  const before = Array.from({ length: 20 }, (_, index) => `line ${index + 1}`).join("\n") + "\n";
  const after = before.replace("line 10\n", "line ten\n");
  assert.equal(unifiedDiff("f.txt", before, after), [
    "--- a/f.txt", "+++ b/f.txt", "@@ -7,7 +7,7 @@",
    " line 7", " line 8", " line 9", "-line 10", "+line ten", " line 11", " line 12", " line 13",
  ].join("\n"));
  assert.equal(unifiedDiff("new.txt", null, "x\n"), ["--- /dev/null", "+++ b/new.txt", "@@ -0,0 +1,1 @@", "+x"].join("\n"));
});
