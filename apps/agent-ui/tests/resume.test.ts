import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, statSync, readdirSync, writeFileSync, realpathSync, mkdirSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawn, execFileSync, type ChildProcess } from "node:child_process";
import { CodexRuntime } from "../src/harness/codex.js";
import { loadThreadId, saveThreadId, threadStatePath } from "../src/thread-state.js";
import { hermeticEnv } from "./hermetic.mjs";

const root = resolve(import.meta.dirname, "..");
const fake = join(root, "tests/fake-codex.mjs");
const temp = () => realpathSync(mkdtempSync(join(tmpdir(), "codex-ui-test-")));
const calls = (file: string) => readFileSync(file, "utf8").trim().split("\n").map((line) => JSON.parse(line));

test("resume sends identity/policy/cwd and maps history even when read fails", async () => {
  const cwd = temp();
  const file = join(cwd, "calls");
  process.env.FAKE_CODEX_CALLS = file;
  process.env.FAKE_READ_FAIL = "1";
  const runtime = new CodexRuntime(fake, cwd, { threadId: "original" });
  try {
    await runtime.start();
    const resume = calls(file).find((call) => call.method === "thread/resume");
    assert.equal(resume.params.threadId, "original");
    assert.equal(resume.params.cwd, cwd);
    assert.equal(resume.params.approvalPolicy, "never");
    assert.equal(resume.params.sandbox, "danger-full-access");
    assert.ok(calls(file).some((call) => call.method === "initialized"));
    assert.ok(!calls(file).some((call) => call.method === "thread/start"));
    assert.equal(runtime.snapshot().thread?.turns.length, 2);
    assert.deepEqual(runtime.snapshot().thread?.turns[0].items.map((item) => item.text), ["remember 0", "recalled 0"]);
    assert.equal(runtime.snapshot(1).thread?.hasOlderItems, true);
  } finally { await runtime.stop(); delete process.env.FAKE_CODEX_CALLS; delete process.env.FAKE_READ_FAIL; }
});

test("resume rejection does not create a replacement thread", async () => {
  const cwd = temp();
  const file = join(cwd, "calls");
  process.env.FAKE_CODEX_CALLS = file;
  const runtime = new CodexRuntime(fake, cwd, { threadId: "missing" });
  try {
    await assert.rejects(runtime.start(), /Failed to resume Codex thread missing.*thread does not exist/);
    assert.equal(runtime.snapshot().ready, false);
    assert.ok(!calls(file).some((call) => call.method === "thread/start"));
  } finally { await runtime.stop(); delete process.env.FAKE_CODEX_CALLS; }
});

test("new thread is persisted privately and resumed by a replacement runtime; explicit ID wins", async () => {
  const cwd = temp();
  const stateFile = threadStatePath(cwd, "ag_test")!;
  const first = new CodexRuntime(fake, cwd, { stateFile });
  await first.start();
  const id = first.thread!.id;
  await first.stop();
  assert.equal(loadThreadId(stateFile), id);
  assert.equal(statSync(stateFile).mode & 0o777, 0o600);
  assert.equal(statSync(join(cwd, ".treer/agents/ag_test")).mode & 0o777, 0o700);
  assert.deepEqual(readdirSync(join(cwd, ".treer/agents/ag_test")), ["agent-ui.json"]);
  const second = new CodexRuntime(fake, cwd, { stateFile });
  try { await second.start(); assert.equal(second.thread!.id, id); assert.equal(second.thread!.turns.length, 2); }
  finally { await second.stop(); }
  const override = new CodexRuntime(fake, cwd, { stateFile, threadId: "override" });
  try { await override.start(); assert.equal(loadThreadId(stateFile), "override"); }
  finally { await override.stop(); }
  writeFileSync(stateFile, "{}");
  assert.throws(() => loadThreadId(stateFile), /refusing to start a new one/);
  assert.throws(() => threadStatePath(cwd, "../other"), /Invalid/);
  assert.equal(threadStatePath(cwd, ""), undefined);
  saveThreadId(undefined, "local");
});

async function launchServer(cwd: string, env: Record<string, string> = {}) {
  const portFile = join(cwd, "port");
  const environment: NodeJS.ProcessEnv = hermeticEnv({ CODEX_UI_CWD: cwd, CODEX_BIN: fake, CODEX_UI_PORT_FILE: portFile, TREER_AGENT_ID: "ag_test", ...env });
  if (!("CODEX_UI_PORT" in env)) delete environment.CODEX_UI_PORT;
  const child = spawn(join(root, "node_modules/.bin/tsx"), [join(root, "src/index.ts")], {
    env: environment,
    stdio: "pipe",
  });
  let output = "";
  child.stderr!.on("data", (chunk) => { output += String(chunk); });
  child.stdout!.on("data", (chunk) => { output += String(chunk); });
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`Server exited: ${output}`);
    try {
      const port = Number(readFileSync(portFile, "utf8"));
      const base = `http://127.0.0.1:${port}`;
      if ((await fetch(base + "/api/health")).ok) return { child, port, base };
    } catch { /* wait for startup */ }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  child.kill();
  throw new Error(`Server did not start: ${output}`);
}
async function stop(child: ChildProcess) {
  if (child.exitCode !== null) return;
  const exited = new Promise((resolve) => child.once("exit", resolve));
  child.kill("SIGTERM");
  await exited;
}

test("read-only file HTTP routes reject traversal and outside symlinks and report truncated previews", async () => {
  const cwd = temp();
  writeFileSync(join(cwd, "large.txt"), "x".repeat(65537));
  mkdirSync(join(cwd, "nested"));
  symlinkSync(tmpdir(), join(cwd, "outside"));
  const server = await launchServer(cwd);
  try {
    const tree = await (await fetch(server.base + "/v1/files/tree?path=")).json();
    assert.ok(tree.entries.some((entry: { name: string }) => entry.name === "nested"));
    assert.ok(!tree.entries.some((entry: { name: string }) => entry.name === "outside"));
    const preview = await (await fetch(server.base + "/v1/files?path=large.txt")).json();
    assert.equal(preview.content.length, 65536);
    assert.equal(preview.truncated, true);
    for (const path of ["../secret", "nested/../large.txt", "outside"]) {
      const denied = await fetch(server.base + "/v1/files?path=" + encodeURIComponent(path));
      assert.equal(denied.status, 403);
      assert.equal((await denied.json()).error.code, "file_access_failed");
    }
    assert.equal((await fetch(server.base + "/v1/files?path=missing")).status, 404);
    assert.equal((await fetch(server.base + "/v1/files/tree?path=large.txt")).status, 400);
    assert.equal((await fetch(server.base + "/v1/files?path=large.txt", { method: "POST", body: "overwrite" })).status, 405);
    assert.equal(readFileSync(join(cwd, "large.txt"), "utf8").length, 65537);
  } finally { await stop(server.child); }
});

test("two dynamic listeners do not conflict and AIS transcript pages resumed turns", async () => {
  const first = await launchServer(temp(), { CODEX_UI_THREAD_ID: "original" });
  let second: Awaited<ReturnType<typeof launchServer>> | undefined;
  try {
    second = await launchServer(temp());
    assert.notEqual(first.port, second.port);
    const page = await (await fetch(first.base + "/v1/transcript?page=0")).json();
    assert.equal(page.page_count, 2);
    assert.equal(page.next_page, 1);
    assert.deepEqual(page.entries.map((item: { content: string }) => item.content), ["remember 0", "recalled 0"]);
    const last = await (await fetch(first.base + "/v1/transcript?cursor=1&limit=2")).json();
    assert.equal(last.next_page, null);
    assert.equal(last.entries[0].content, "remember 1");
  } finally { await stop(first.child); if (second) await stop(second.child); }
});

test("switch dry-run prints exact self startup argv without writing or restarting", () => {
  const cwd = temp();
  const bin = join(cwd, "treer");
  const log = join(cwd, "calls");
  writeFileSync(bin, `#!/bin/sh\nprintf '%s\\n' "$*" >> '${log}'\nif [ "$1" = whoami ]; then printf '%s\\n' '{"agent":{"agent_id":"ag_test"}}'; else exit 99; fi\n`, { mode: 0o700 });
  const result = execFileSync(join(root, "scripts/switch.sh"), ["--thread-id", "original", "--dry-run"], {
    cwd, encoding: "utf8", env: hermeticEnv({ PATH: `${cwd}:${process.env.PATH}`, TREER_AGENT_ID: "ag_test", TREER_WORKLOAD_CREDENTIAL: "test", TREER_AGENT_SERVER_URL: "test" }),
  });
  const plan = JSON.parse(result);
  assert.deepEqual(plan.startup_command, ["treer", "agent", "startup", "set", "--cwd", ".", "--", join(root, "scripts/treer-agent.sh"), "--harness", "codex", "--session-id", "original", "--cwd", cwd]);
  assert.equal(readFileSync(log, "utf8"), "whoami\n");
  assert.deepEqual(readdirSync(cwd).sort(), ["calls", "treer"]);
});

test("launcher CLI overrides env thread and registers the actual dynamic listener port", async () => {
  const cwd = temp();
  const log = join(cwd, "treer-calls");
  const code = join(cwd, "treer");
  writeFileSync(code, `#!/bin/sh\nprintf '%s\\n' "$*" >> '${log}'\nprintf '%s\\n' '{}'\n`, { mode: 0o700 });
  const child = spawn(join(root, "scripts/treer-agent.sh"), ["--thread-id", "from-cli", "--cwd", cwd], {
    cwd, env: hermeticEnv({ HOME: cwd, PATH: `${cwd}:${process.env.PATH}`, CODEX_BIN: fake,
      CODEX_UI_THREAD_ID: "from-env", CODEX_UI_PORT: "0", TREER_AGENT_ID: "ag_shell" }), stdio: "pipe",
  });
  let output = "";
  child.stderr!.on("data", (chunk) => { output += String(chunk); });
  const deadline = Date.now() + 10000;
  try {
    let port = 0;
    while (Date.now() < deadline) {
      if (child.exitCode !== null) throw new Error(`launcher exited: ${output}`);
      try {
        const match = readFileSync(log, "utf8").match(/interface register --port (\d+)/);
        if (match) { port = Number(match[1]); break; }
      } catch { /* wait for registration */ }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    assert.ok(port > 0, output);
    const state = await (await fetch(`http://127.0.0.1:${port}/api/state`)).json();
    assert.equal(state.thread.id, "from-cli");
    assert.equal(state.thread.cwd, cwd);
    assert.equal(loadThreadId(threadStatePath(cwd, "ag_shell")), "from-cli");
    assert.match(readFileSync(log, "utf8"), /--ui-path \/\n/);
  } finally { await stop(child); }
});

test("failed resume exits launcher before interface registration", async () => {
  const cwd = temp();
  const log = join(cwd, "treer-calls");
  writeFileSync(join(cwd, "treer"), `#!/bin/sh\nprintf '%s\\n' "$*" >> '${log}'\n`, { mode: 0o700 });
  const child = spawn(join(root, "scripts/treer-agent.sh"), ["--thread-id", "missing"], {
    cwd, env: hermeticEnv({ HOME: cwd, PATH: `${cwd}:${process.env.PATH}`, CODEX_BIN: fake, CODEX_UI_PORT: "0" }), stdio: "pipe",
  });
  let output = "";
  child.stderr!.on("data", (chunk) => { output += String(chunk); });
  const code = await new Promise((resolve) => child.once("exit", resolve));
  assert.equal(code, 1);
  assert.match(output, /Failed to resume Codex thread missing/);
  assert.ok(!readFileSync(log, "utf8").includes("interface register"));
});

test("switch saves startup and detaches a self-only restart with private result/log files", async () => {
  const cwd = temp();
  const log = join(cwd, "calls");
  writeFileSync(join(cwd, "treer"), `#!/bin/sh\nprintf '%s\\n' "$*" >> '${log}'\nif [ "$1" = whoami ]; then printf '%s\\n' '{"agent":{"agent_id":"ag_test"}}'; else printf '%s\\n' '{}'; fi\n`, { mode: 0o700 });
  const output = execFileSync(join(root, "scripts/switch.sh"), ["--thread-id", "original"], {
    cwd, encoding: "utf8", env: hermeticEnv({ PATH: `${cwd}:${process.env.PATH}`, TREER_AGENT_ID: "ag_test", TREER_WORKLOAD_CREDENTIAL: "test", TREER_AGENT_SERVER_URL: "test" }),
  });
  assert.match(output, /detached self restart queued/);
  const parent = join(cwd, ".treer/agents/ag_test/agent-ui-switches");
  const state = join(parent, readdirSync(parent)[0]);
  const result = join(state, "result.json");
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    try { readFileSync(result); break; } catch { await new Promise((resolve) => setTimeout(resolve, 20)); }
  }
  assert.equal(JSON.parse(readFileSync(result, "utf8")).restart_exit_code, 0);
  assert.equal(statSync(state).mode & 0o777, 0o700);
  for (const file of ["plan.json", "restart.log", "result.json"]) {
    assert.equal(statSync(join(state, file)).mode & 0o777, 0o600);
  }
  const recorded = readFileSync(log, "utf8").trim().split("\n");
  assert.equal(recorded[0], "whoami");
  assert.match(recorded[1], /^agent startup set --cwd \. -- /);
  assert.equal(recorded[2], "agent restart self");
  assert.equal(recorded.length, 3);
});

test("a turn left in progress by an earlier restart does not make an idle thread look running", async () => {
  const cwd = temp();
  process.env.FAKE_CODEX_STALE = "1";
  process.env.FAKE_CODEX_STATUS = "idle";
  const runtime = new CodexRuntime(fake, cwd, { threadId: "original" });
  try {
    await runtime.start();
    const thread = runtime.snapshot().thread!;
    assert.equal(thread.status, "idle");
    assert.equal(thread.activeTurnId, null);
    assert.equal(thread.turns[0].status, "interrupted");
  } finally { await runtime.stop(); delete process.env.FAKE_CODEX_STALE; delete process.env.FAKE_CODEX_STATUS; }
});

test("only the newest in-progress turn is live, and a stale steer target is corrected", async () => {
  const cwd = temp();
  const file = join(cwd, "calls");
  process.env.FAKE_CODEX_CALLS = file;
  process.env.FAKE_CODEX_STALE = "1";
  process.env.FAKE_CODEX_LIVE = "1";
  process.env.FAKE_CODEX_STATUS = "active";
  const runtime = new CodexRuntime(fake, cwd, { threadId: "original" });
  try {
    await runtime.start();
    const thread = runtime.snapshot().thread!;
    assert.equal(thread.status, "running");
    assert.equal(thread.activeTurnId, "turn-live");
    assert.deepEqual(thread.turns.map((turn) => turn.status), ["interrupted", "completed", "inProgress"]);
    // A steer aimed at a stale ID is retried at the turn Codex reports.
    runtime.thread!.activeTurnId = "turn-0";
    await runtime.prompt("steer me");
    const steers = calls(file).filter((call) => call.method === "turn/steer");
    assert.deepEqual(steers.map((call) => call.params.expectedTurnId), ["turn-0", "turn-live"]);
  } finally {
    await runtime.stop();
    for (const key of ["FAKE_CODEX_CALLS", "FAKE_CODEX_STALE", "FAKE_CODEX_LIVE", "FAKE_CODEX_STATUS"]) delete process.env[key];
  }
});
