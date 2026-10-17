import { test } from "node:test";
import { get } from "node:http";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, statSync, readdirSync, writeFileSync, realpathSync, mkdirSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawn, execFileSync, type ChildProcess } from "node:child_process";
import { CodexRuntime } from "../src/harness/codex.js";
import { loadThreadId, saveThreadId, threadStatePath } from "../src/thread-state.js";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";

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

test("session archive HTTP route rejects browser origins and DNS-rebinding Hosts before touching native storage", async () => {
  const server = await launchServer(temp());
  try {
    const denied: Record<string, string>[] = [{ origin: "https://untrusted.example" }, { origin: "null" }];
    for (const headers of denied) {
      assert.equal((await fetch(server.base + "/api/session/export", { headers })).status, 403);
    }
    const rebound = await new Promise<number | undefined>((resolve, reject) => {
      get(server.base + "/api/session/export", { headers: { host: "untrusted.example" } }, (response) => { response.resume(); resolve(response.statusCode); }).on("error", reject);
    });
    assert.equal(rebound, 403);
    // Fake Codex has no native UUID/file: a local client reaches the adapter,
    // rather than being rejected by the browser-boundary guard.
    assert.equal((await fetch(server.base + "/api/session/export")).status, 400);
  } finally { await stop(server.child); }
});

test("file download streams regular files with safe headers and keeps the working-directory confinement", async () => {
  const cwd = temp();
  writeFileSync(join(cwd, "plain.txt"), "hello download\n");
  writeFileSync(join(cwd, "résumé 日本語.txt"), "unicode name");
  writeFileSync(join(cwd, 'quote";x.txt'), "quote name");
  writeFileSync(join(cwd, "empty.bin"), "");
  mkdirSync(join(cwd, "nested"));
  writeFileSync(join(cwd, "nested", "inner.txt"), "inner");
  symlinkSync(join(cwd, "nested", "inner.txt"), join(cwd, "inside-link"));
  const elsewhere = temp();
  writeFileSync(join(elsewhere, "secret.txt"), "not yours");
  symlinkSync(elsewhere, join(cwd, "outside"));
  symlinkSync(join(elsewhere, "secret.txt"), join(cwd, "outside-file"));
  if (process.platform !== "win32") execFileSync("mkfifo", [join(cwd, "pipe")]);
  const server = await launchServer(cwd);
  const get = (path: string, init?: RequestInit) => fetch(server.base + "/v1/files/download?path=" + encodeURIComponent(path), init);
  try {
    const plain = await get("plain.txt");
    assert.equal(plain.status, 200);
    assert.equal(await plain.text(), "hello download\n");
    assert.equal(plain.headers.get("content-type"), "application/octet-stream");
    assert.equal(plain.headers.get("content-length"), "15");
    assert.equal(plain.headers.get("x-content-type-options"), "nosniff");
    assert.equal(plain.headers.get("cache-control"), "no-store");
    assert.match(plain.headers.get("content-disposition")!, /^attachment; filename="plain\.txt"; filename\*=UTF-8''plain\.txt$/);
    // Non-ASCII names: ASCII-only fallback plus an RFC 5987 UTF-8 filename*; quotes and semicolons never break the header.
    const unicode = await get("résumé 日本語.txt");
    assert.equal(await unicode.text(), "unicode name");
    const disposition = unicode.headers.get("content-disposition")!;
    assert.match(disposition, /^attachment; filename="[\x20-\x7e]+"; filename\*=UTF-8''r%C3%A9sum%C3%A9%20%E6%97%A5%E6%9C%AC%E8%AA%9E\.txt$/);
    const quoted = (await get('quote";x.txt')).headers.get("content-disposition")!;
    assert.match(quoted, /^attachment; filename="quote__x\.txt"; filename\*=UTF-8''quote%22%3Bx\.txt$/);
    assert.equal((await (await get("empty.bin")).arrayBuffer()).byteLength, 0);
    // Internal symlinks work, like the preview.
    assert.equal(await (await get("inside-link")).text(), "inner");
    // Confinement negatives and non-regular files.
    for (const path of ["../secret", "nested/../plain.txt", "/etc/passwd", "outside", "outside-file", "outside/secret.txt"]) {
      const denied = await get(path);
      assert.equal(denied.status, 403, path);
      assert.equal((await denied.json()).error.code, "file_access_failed");
    }
    assert.equal((await get("nested")).status, 400);
    if (process.platform !== "win32") assert.equal((await get("pipe")).status, 400);
    assert.equal((await get("")).status, 400);
    assert.equal((await get("missing")).status, 404);
    const post = await get("plain.txt", { method: "POST", body: "x" });
    assert.equal(post.status, 405);
    assert.equal(post.headers.get("allow"), "GET");
    assert.equal(readFileSync(join(cwd, "plain.txt"), "utf8"), "hello download\n");
  } finally { await stop(server.child); }
});

test("file download enforces the 1 GiB limit from the open descriptor, before any byte", { skip: process.platform === "win32" }, async () => {
  const cwd = temp();
  const limit = 1024 ** 3;
  execFileSync("truncate", ["-s", String(limit), join(cwd, "exact.bin")]);
  execFileSync("truncate", ["-s", String(limit + 1), join(cwd, "over.bin")]);
  const server = await launchServer(cwd);
  try {
    // Exactly at the limit is allowed; look at the headers and drop the connection instead of moving a GiB.
    const exact = await new Promise<import("node:http").IncomingMessage>((resolve, reject) => { const request = get(server.base + "/v1/files/download?path=exact.bin", (response) => { response.pause(); resolve(response); }); request.on("error", reject); });
    assert.equal(exact.statusCode, 200);
    assert.equal(exact.headers["content-length"], String(limit));
    exact.destroy();
    const over = await fetch(server.base + "/v1/files/download?path=over.bin");
    assert.equal(over.status, 413);
    assert.equal(over.headers.get("content-disposition"), null);
    const body = await over.json();
    assert.equal(body.error.code, "file_too_large");
    assert.equal(body.error.limit_bytes, limit);
    assert.equal(body.error.size, limit + 1);
    assert.match(body.error.message, /1 GiB/);
  } finally { await stop(server.child); }
});

test("file download caps concurrent downloads at two and frees the slot when a client goes away", { skip: process.platform === "win32" }, async () => {
  const cwd = temp();
  execFileSync("truncate", ["-s", String(256 * 1024 * 1024), join(cwd, "big.bin")]);
  writeFileSync(join(cwd, "small.txt"), "small");
  const server = await launchServer(cwd);
  const held: ReturnType<typeof get>[] = [];
  const hold = () => new Promise<import("node:http").IncomingMessage>((resolve, reject) => {
    const request = get(server.base + "/v1/files/download?path=big.bin", (response) => { response.pause(); resolve(response); });
    request.on("error", reject);
    held.push(request);
  });
  try {
    const first = await hold(), second = await hold();
    assert.equal(first.statusCode, 200); assert.equal(second.statusCode, 200);
    const third = await fetch(server.base + "/v1/files/download?path=small.txt");
    assert.equal(third.status, 429);
    assert.equal(third.headers.get("retry-after"), "2");
    assert.equal((await third.json()).error.code, "too_many_downloads");
    first.destroy();
    let freed = false;
    for (let attempt = 0; attempt < 50 && !freed; attempt++) {
      const retry = await fetch(server.base + "/v1/files/download?path=small.txt");
      freed = retry.status === 200 && (await retry.text()) === "small";
      if (!freed) await new Promise((resolve) => setTimeout(resolve, 100));
    }
    assert.ok(freed, "a slot is released after a client disconnects");
    // Other routes are not affected by the download cap.
    assert.equal((await fetch(server.base + "/v1/files/tree?path=")).status, 200);
  } finally { for (const request of held) request.destroy(); await stop(server.child); }
});

test("file download destroys a response whose client stopped reading (write-idle timeout) and frees its slot", { skip: process.platform === "win32" }, async () => {
  const cwd = temp();
  execFileSync("truncate", ["-s", String(512 * 1024 * 1024), join(cwd, "big.bin")]);
  writeFileSync(join(cwd, "small.txt"), "small");
  const server = await launchServer(cwd, { AGENT_UI_DOWNLOAD_IDLE_MS: "400" });
  const held: ReturnType<typeof get>[] = [];
  // Two clients that accept the response and never read it fill both download slots.
  const stall = () => new Promise<void>((resolve, reject) => {
    const request = get(server.base + "/v1/files/download?path=big.bin", (response) => { response.pause(); response.on("error", () => {}); resolve(); });
    request.on("error", () => {}); request.on("socket", (socket) => socket.on("error", () => {}));
    held.push(request); setTimeout(() => reject(new Error("no response")), 5000);
  });
  try {
    await stall(); await stall();
    // A paused client stream does not report the server closing the connection, so the observable effect is the freed slot:
    // right now both slots are taken (429); once the idle timeout destroys the stalled responses a download works again.
    assert.equal((await fetch(server.base + "/v1/files/download?path=small.txt")).status, 429);
    let status = 429;
    for (let attempt = 0; attempt < 60 && status !== 200; attempt++) {
      status = (await fetch(server.base + "/v1/files/download?path=small.txt")).status;
      if (status !== 200) await new Promise((resolve) => setTimeout(resolve, 100));
    }
    assert.equal(status, 200, "the stalled downloads were destroyed and their slots released");
  } finally { for (const request of held) request.destroy(); await stop(server.child); }
});

test("file download ends a slow-drip download after the overall duration cap and frees its slot", { skip: process.platform === "win32" }, async () => {
  const cwd = temp();
  execFileSync("truncate", ["-s", String(512 * 1024 * 1024), join(cwd, "big.bin")]);
  writeFileSync(join(cwd, "small.txt"), "small");
  // The idle timeout stays at its default (60 s): these clients keep the socket active, only the duration cap can stop them.
  const server = await launchServer(cwd, { AGENT_UI_DOWNLOAD_MAX_MS: "700" });
  const held: ReturnType<typeof get>[] = [];
  const drip = () => new Promise<void>((resolve, reject) => {
    const request = get(server.base + "/v1/files/download?path=big.bin", (response) => {
      response.on("error", () => {});
      // Read a little every 50 ms: never idle, never finished.
      const timer = setInterval(() => { response.read(1024); }, 50);
      response.once("close", () => clearInterval(timer));
      response.socket.once("close", () => clearInterval(timer));
      response.pause(); resolve();
    });
    request.on("error", () => {}); request.on("socket", (socket) => socket.on("error", () => {}));
    held.push(request); setTimeout(() => reject(new Error("no response")), 5000);
  });
  try {
    await drip(); await drip();
    assert.equal((await fetch(server.base + "/v1/files/download?path=small.txt")).status, 429);
    let status = 429;
    for (let attempt = 0; attempt < 60 && status !== 200; attempt++) {
      status = (await fetch(server.base + "/v1/files/download?path=small.txt")).status;
      if (status !== 200) await new Promise((resolve) => setTimeout(resolve, 100));
    }
    assert.equal(status, 200, "the trickling downloads were cut off by the duration cap");
  } finally { for (const request of held) request.destroy(); await stop(server.child); }
});

test("file download streams a large file with bounded server memory", { skip: process.platform === "win32" }, async () => {
  const cwd = temp();
  const size = 192 * 1024 * 1024;
  execFileSync("truncate", ["-s", String(size), join(cwd, "big.bin")]);
  const server = await launchServer(cwd);
  const rss = () => Number(execFileSync("ps", ["-o", "rss=", "-p", String(server.child.pid)], { encoding: "utf8" }).trim()) * 1024;
  try {
    const before = rss();
    let received = 0, peak = before;
    // node:http streams chunk by chunk (the shared localFetch helper buffers the whole body), so the server is sampled mid-transfer.
    await new Promise<void>((resolve, reject) => {
      const request = get(server.base + "/v1/files/download?path=big.bin", (response) => {
        assert.equal(response.statusCode, 200);
        assert.equal(response.headers["content-length"], String(size));
        response.on("data", (chunk: Buffer) => {
          received += chunk.length;
          if (received % (16 * 1024 * 1024) < chunk.length) peak = Math.max(peak, rss());
        });
        response.on("end", resolve); response.on("error", reject);
      });
      request.on("error", reject);
    });
    assert.equal(received, size);
    // A full read into memory would add 192 MiB; streaming stays far below that.
    assert.ok(peak - before < 80 * 1024 * 1024, `RSS grew by ${((peak - before) / 1048576).toFixed(0)} MiB`);
  } finally { await stop(server.child); }
});

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

test("goal contract exposes null when unsupported and mutations return 501", async () => {
  const server = await launchServer(temp(), { FAKE_CODEX_NO_GOAL: "1" });
  try {
    const state = await (await fetch(server.base + "/api/state")).json();
    assert.equal(state.goal, null);
    assert.equal(state.harness.capabilities.goal, null);
    for (const method of ["PUT", "DELETE"]) {
      const response = await fetch(server.base + "/api/goal", { method, ...(method === "PUT" ? { body: JSON.stringify({ objective: "test" }) } : {}) });
      assert.equal(response.status, 501);
      assert.match((await response.json()).error, /does not support goals/);
    }
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
  symlinkSync(fake, join(cwd, "codex"));
  const bin = join(cwd, "treer");
  const log = join(cwd, "calls");
  writeFileSync(bin, `#!/bin/sh\nprintf '%s\\n' "$*" >> '${log}'\nif [ "$1" = whoami ]; then printf '%s\\n' '{"agent":{"agent_id":"ag_test"}}'; else exit 99; fi\n`, { mode: 0o700 });
  const result = execFileSync(join(root, "scripts/switch.sh"), ["--thread-id", "original", "--dry-run"], {
    cwd, encoding: "utf8", env: hermeticEnv({ PATH: `${cwd}:${process.env.PATH}`, TREER_AGENT_ID: "ag_test", TREER_WORKLOAD_CREDENTIAL: "test", TREER_AGENT_SERVER_URL: "test" }),
  });
  const plan = JSON.parse(result);
  assert.deepEqual(plan.startup_command, ["treer", "agent", "startup", "set", "--cwd", ".", "--", join(root, "scripts/treer-agent.sh"), "--harness", "codex", "--session-id", "original", "--cwd", cwd]);
  assert.equal(readFileSync(log, "utf8"), "whoami\n");
  assert.deepEqual(readdirSync(cwd).sort(), ["calls", "codex", "treer"]);
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
  symlinkSync(fake, join(cwd, "codex"));
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
  let receipt: { restart_exit_code: number } | undefined;
  while (Date.now() < deadline) {
    try {
      // The detached Python writer creates the file before writing its JSON.
      // Presence (or even a successful read) is not a completion receipt.
      receipt = JSON.parse(readFileSync(result, "utf8"));
      break;
    } catch (error) {
      if (!(error instanceof SyntaxError) && (error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
  }
  assert.ok(receipt, "detached switch must publish a complete JSON receipt within 5 seconds");
  assert.equal(receipt.restart_exit_code, 0);
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

test("/v1/prompts accepts the body Treer's Controller sends, and delivery selects steer or queue", async () => {
  const server = await launchServer(temp(), { CODEX_UI_THREAD_ID: "original" });
  const instance = await fetch(server.base + "/v1/status").then((response) => response.json());
  const post = (body: unknown) => fetch(server.base + "/v1/prompts", { method: "POST", headers: { "content-type": "application/json", host: "agent-interface", "x-treer-interface-source": "controller-api", "x-treer-agent-id": "ag_test", "x-treer-interface-instance": instance.interface_instance_id }, body: JSON.stringify(body) });
  try {
    // Exactly what crates/treer-agent-server/src/agent_interface.rs submits.
    const controller = await post({ operation_id: "op-controller", text: "hello", mode: "prompt" });
    assert.equal(controller.status, 202, await controller.clone().text());
    const queued = await post({ operation_id: "op-queue", text: "later", delivery: "queue" });
    assert.equal(queued.status, 202, await queued.clone().text());
    const invalid = await post({ operation_id: "op-bad", text: "x", delivery: "now" });
    assert.equal(invalid.status, 400);
    assert.match(await invalid.text(), /must be steer or queue/);
  } finally { await stop(server.child); }
});
