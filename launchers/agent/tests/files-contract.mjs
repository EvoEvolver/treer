// File routes contract (#158 M1/M2, #165): the same assertions run against the Node launcher (default) or the Rust launcher
// (LAUNCHER_BACKEND=rust RUST_LAUNCHER_BIN=...): `node --test tests/files-contract.mjs`.
import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { get, Agent } from "node:http";
import { mkdtemp, mkdir, writeFile, symlink, readFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";
import { fakePrivateState } from "./fake-private-state.mjs";
import { backend, spawnLauncher } from "./launcher-backend.mjs";

const app = resolve(import.meta.dirname, "..");
const unix = process.platform !== "win32";
async function boot(t, files, extra = {}) {
  const cwd = await realpath(await mkdtemp(join(tmpdir(), "files-contract-")));
  const privateRoot = await realpath(await mkdtemp(join(tmpdir(), "files-contract-private-")));
  const controller = await fakePrivateState("ag_files_contract");
  await files(cwd);
  const env = hermeticEnv({ UI_DEV_FIXTURE: randomUUID(), TREER_AGENT_PRIVATE_STATE_DIR: privateRoot, TREER_AGENT_ID: "ag_files_contract", TREER_AGENT_SERVER_URL: controller.url, TREER_WORKLOAD_CREDENTIAL: "fixture-only", AGENT_UI_MESSAGE_WATCH: "0", AGENT_UI_INSTANCE_ID: "files-contract", CODEX_UI_PUBLIC_DIR: join(app, "public"), CODEX_UI_PORT: "0", CODEX_UI_CWD: cwd, CODEX_UI_PORT_FILE: join(cwd, ".port-file"), CODEX_BIN: join(app, "tests/fake-codex.mjs"), ...extra });
  const child = spawnLauncher(app, env);
  let logs = ""; child.stderr.on("data", (c) => { logs += c; }); child.stdout.on("data", (c) => { logs += c; });
  let base;
  for (let attempt = 0; attempt < 200 && !base; attempt++) {
    if (child.exitCode !== null) throw new Error(`launcher exited: ${logs}`);
    try { const candidate = `http://127.0.0.1:${Number(await readFile(join(cwd, ".port-file"), "utf8"))}`; if ((await fetch(candidate + "/api/health")).ok) base = candidate; } catch { /* starting */ }
    if (!base) await new Promise((r) => setTimeout(r, 50));
  }
  assert.ok(base, logs);
  t.after(async () => { if (child.exitCode === null) { const exited = new Promise((r) => child.once("exit", r)); child.kill("SIGTERM"); await exited; } await controller.close(); await rm(cwd, { recursive: true, force: true }); await rm(privateRoot, { recursive: true, force: true }); });
  return { base, cwd, child, logs: () => logs, rss: () => Number(execFileSync("ps", ["-o", "rss=", "-p", String(child.pid)], { encoding: "utf8" }).trim()) * 1024 };
}
const sparse = (path, size) => execFileSync("truncate", ["-s", String(size), path]);
const stream = (url, options = {}) => new Promise((resolve, reject) => { const request = get(url, options, (response) => { response.pause(); response.on("error", () => {}); resolve({ response, request }); }); request.on("error", (error) => { if (!options.tolerate) reject(error); }); });

test(`[${backend}] tree and preview: sorted listing, hidden outside symlinks, 64 KiB preview, error shapes`, async (t) => {
  const s = await boot(t, async (cwd) => {
    await mkdir(join(cwd, "src")); await mkdir(join(cwd, "Docs"));
    await writeFile(join(cwd, "src", "a.txt"), "inner"); await writeFile(join(cwd, "README.md"), "hello"); await writeFile(join(cwd, "large.txt"), "x".repeat(65537));
    const elsewhere = await realpath(await mkdtemp(join(tmpdir(), "files-contract-out-"))); await writeFile(join(elsewhere, "secret"), "no");
    await symlink(elsewhere, join(cwd, "outside")); await symlink(join(cwd, "src", "a.txt"), join(cwd, "inside-link"));
  });
  const tree = await (await fetch(s.base + "/v1/files/tree?path=")).json();
  const names = tree.entries.map((e) => `${e.kind[0]}:${e.name}`).filter((n) => !n.endsWith(".port-file") && !n.endsWith(".treer"));
  assert.deepEqual(names.slice(0, 2), ["d:Docs", "d:src"], "directories first, case-insensitive order");
  assert.ok(!tree.entries.some((e) => e.name === "outside"), "an outside symlink is not listed");
  assert.ok(tree.entries.some((e) => e.name === "inside-link"), "an inside symlink is listed");
  assert.equal(tree.entries.find((e) => e.name === "README.md").size, 5);
  assert.equal(tree.entries.find((e) => e.name === "src").size, null);
  const preview = await (await fetch(s.base + "/v1/files?path=large.txt")).json();
  assert.equal(preview.content.length, 65536); assert.equal(preview.truncated, true); assert.equal(preview.size, 65537); assert.equal(preview.name, "large.txt");
  assert.equal((await (await fetch(s.base + "/v1/files?path=src/a.txt")).json()).path, "src/a.txt");
  for (const path of ["../secret", "src/../README.md", "outside", "outside/secret", "/etc/hosts"]) { const denied = await fetch(s.base + "/v1/files?path=" + encodeURIComponent(path)); assert.equal(denied.status, 403, path); assert.equal((await denied.json()).error.code, "file_access_failed"); }
  assert.equal((await fetch(s.base + "/v1/files?path=missing")).status, 404);
  assert.equal((await fetch(s.base + "/v1/files/tree?path=large.txt")).status, 400);
  const post = await fetch(s.base + "/v1/files?path=large.txt", { method: "POST", body: "x" }); assert.equal(post.status, 405); assert.equal(post.headers.get("allow"), "GET");
});

test(`[${backend}] download: bytes, safe headers, names, confinement`, async (t) => {
  const s = await boot(t, async (cwd) => {
    await writeFile(join(cwd, "plain.txt"), "hello download\n"); await writeFile(join(cwd, "résumé 日本語.txt"), "unicode name"); await writeFile(join(cwd, 'quote";x.txt'), "q"); await writeFile(join(cwd, "empty.bin"), "");
    await mkdir(join(cwd, "nested")); await writeFile(join(cwd, "nested", "inner.txt"), "inner"); await symlink(join(cwd, "nested", "inner.txt"), join(cwd, "inside-link"));
    const elsewhere = await realpath(await mkdtemp(join(tmpdir(), "files-contract-out-"))); await writeFile(join(elsewhere, "secret.txt"), "no");
    await symlink(elsewhere, join(cwd, "outside")); await symlink(join(elsewhere, "secret.txt"), join(cwd, "outside-file"));
    if (unix) execFileSync("mkfifo", [join(cwd, "pipe")]);
  });
  const dl = (path, init) => fetch(s.base + "/v1/files/download?path=" + encodeURIComponent(path), init);
  const plain = await dl("plain.txt");
  assert.equal(plain.status, 200); assert.equal(await plain.text(), "hello download\n");
  assert.equal(plain.headers.get("content-type"), "application/octet-stream"); assert.equal(plain.headers.get("content-length"), "15");
  assert.equal(plain.headers.get("x-content-type-options"), "nosniff"); assert.equal(plain.headers.get("cache-control"), "no-store");
  assert.match(plain.headers.get("content-disposition"), /^attachment; filename="plain\.txt"; filename\*=UTF-8''plain\.txt$/);
  assert.match((await dl("résumé 日本語.txt")).headers.get("content-disposition"), /^attachment; filename="[\x20-\x7e]+"; filename\*=UTF-8''r%C3%A9sum%C3%A9%20%E6%97%A5%E6%9C%AC%E8%AA%9E\.txt$/);
  assert.match((await dl('quote";x.txt')).headers.get("content-disposition"), /^attachment; filename="quote__x\.txt"; filename\*=UTF-8''quote%22%3Bx\.txt$/);
  assert.equal((await (await dl("empty.bin")).arrayBuffer()).byteLength, 0);
  assert.equal(await (await dl("inside-link")).text(), "inner");
  for (const path of ["../secret", "nested/../plain.txt", "/etc/passwd", "outside", "outside-file", "outside/secret.txt"]) { const denied = await dl(path); assert.equal(denied.status, 403, path); assert.equal((await denied.json()).error.code, "file_access_failed"); }
  assert.equal((await dl("nested")).status, 400); if (unix) assert.equal((await dl("pipe")).status, 400);
  assert.equal((await dl("")).status, 400); assert.equal((await dl("missing")).status, 404);
  const post = await dl("plain.txt", { method: "POST", body: "x" }); assert.equal(post.status, 405); assert.equal(post.headers.get("allow"), "GET");
});

test(`[${backend}] download: cross-site refusal and the 1 GiB limit from the open descriptor`, { skip: !unix }, async (t) => {
  const limit = 1024 ** 3;
  const s = await boot(t, async (cwd) => { await writeFile(join(cwd, "plain.txt"), "p"); sparse(join(cwd, "exact.bin"), limit); sparse(join(cwd, "over.bin"), limit + 1); });
  const ui = { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_files_contract", "x-treer-interface-instance": "files-contract" };
  const withSite = (site) => fetch(s.base + "/v1/files/download?path=plain.txt", { headers: { ...ui, ...(site ? { "sec-fetch-site": site, "sec-fetch-mode": "navigate" } : {}) } });
  const cross = await withSite("cross-site"); assert.equal(cross.status, 403); assert.equal((await cross.json()).error.code, "cross_site_download");
  for (const site of ["same-origin", "same-site", "none", undefined]) { const ok = await withSite(site); assert.equal(ok.status, 200, String(site)); await ok.arrayBuffer(); }
  const { response, request } = await stream(s.base + "/v1/files/download?path=exact.bin");
  assert.equal(response.statusCode, 200); assert.equal(response.headers["content-length"], String(limit)); request.destroy(); response.destroy();
  const over = await fetch(s.base + "/v1/files/download?path=over.bin");
  assert.equal(over.status, 413); assert.equal(over.headers.get("content-disposition"), null);
  const body = await over.json(); assert.equal(body.error.code, "file_too_large"); assert.equal(body.error.limit_bytes, limit); assert.equal(body.error.size, limit + 1); assert.match(body.error.message, /1 GiB/);
});

test(`[${backend}] download: two slots, freed when a client goes away; idle and duration timeouts free stalled/trickling clients`, { skip: !unix }, async (t) => {
  const s = await boot(t, async (cwd) => { sparse(join(cwd, "big.bin"), 512 * 1024 * 1024); await writeFile(join(cwd, "small.txt"), "small"); }, { AGENT_UI_DOWNLOAD_IDLE_MS: "400", AGENT_UI_DOWNLOAD_MAX_MS: "1500" });
  const url = s.base + "/v1/files/download?path=big.bin", small = s.base + "/v1/files/download?path=small.txt";
  const held = [];
  t.after(() => { for (const { request, response } of held) { request.destroy(); response.destroy(); } });
  held.push(await stream(url, { tolerate: true }), await stream(url, { tolerate: true }));
  const third = await fetch(small); assert.equal(third.status, 429); assert.equal(third.headers.get("retry-after"), "2"); assert.equal((await third.json()).error.code, "too_many_downloads");
  // A stalled reader is destroyed after the idle time and its slot freed; other routes were never affected.
  assert.equal((await fetch(s.base + "/v1/files/tree?path=")).status, 200);
  let status = 429; for (let i = 0; i < 80 && status !== 200; i++) { status = (await fetch(small)).status; if (status !== 200) await new Promise((r) => setTimeout(r, 100)); }
  assert.equal(status, 200, "the stalled downloads were destroyed and their slots released");
  // Disconnect frees a slot at once.
  const a = await stream(url, { tolerate: true }), b = await stream(url, { tolerate: true }); held.push(a, b);
  assert.equal((await fetch(small)).status, 429);
  a.request.destroy(); a.response.destroy();
  status = 429; for (let i = 0; i < 50 && status !== 200; i++) { status = (await fetch(small)).status; if (status !== 200) await new Promise((r) => setTimeout(r, 50)); }
  assert.equal(status, 200, "a slot is released when a client disconnects");
});

test(`[${backend}] download: bounded server memory, and many downloads over one keep-alive connection`, { skip: !unix }, async (t) => {
  const size = 192 * 1024 * 1024;
  const s = await boot(t, async (cwd) => { sparse(join(cwd, "big.bin"), size); await writeFile(join(cwd, "small.txt"), "small"); });
  const before = s.rss(); let received = 0, peak = before;
  await new Promise((resolve, reject) => {
    get(s.base + "/v1/files/download?path=big.bin", (response) => { assert.equal(response.statusCode, 200); response.on("data", (chunk) => { received += chunk.length; if (received % (16 * 1024 * 1024) < chunk.length) peak = Math.max(peak, s.rss()); }); response.on("end", resolve); response.on("error", reject); }).on("error", reject);
  });
  assert.equal(received, size);
  assert.ok(peak - before < 80 * 1024 * 1024, `RSS grew by ${((peak - before) / 1048576).toFixed(0)} MiB`);
  const agent = new Agent({ keepAlive: true, maxSockets: 1 }); const sockets = new Set();
  t.after(() => agent.destroy());
  for (let i = 0; i < 15; i++) {
    const body = await new Promise((resolve, reject) => { const request = get(s.base + "/v1/files/download?path=small.txt", { agent }, (response) => { let text = ""; response.on("data", (c) => { text += c; }); response.on("end", () => resolve(text)); response.on("error", reject); }); request.on("socket", (socket) => sockets.add(socket)); request.on("error", reject); });
    assert.equal(body, "small");
  }
  assert.equal(sockets.size, 1, "one keep-alive connection served all downloads");
  assert.ok(!/MaxListenersExceededWarning/.test(s.logs()), s.logs());
});
