// Optional browser regression check; uses the existing web Playwright dependency.
// Build the runtime first, then: node launchers/acp/tests/builtin_ui_browser.mjs
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { copyFile, mkdtemp, mkdir, readFile, writeFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const require = createRequire(process.env.TREER_UI_TEST_MODULE_ROOT || new URL("../../../web/package.json", import.meta.url));
const { chromium } = require("playwright");
const { expect } = require("@playwright/test");
const scratch = await mkdtemp(`${tmpdir()}/treer-builtin-ui-`);
const cwd = `${scratch}/cwd`;
await mkdir(`${cwd}/src`, { recursive: true });
await writeFile(`${cwd}/src/hello & #.txt`, "<script>window.injected = true</script>\nHello!");
await writeFile(`${cwd}/large.txt`, "x".repeat(70000));
await mkdir(`${scratch}/bin`);
await writeFile(`${scratch}/bin/treer`, `#!/bin/sh\nprintf '%s\\n' "$@" > '${scratch}/registration.txt'\n`, { mode: 0o755 });
// A sibling Treer stub takes precedence over the runtime's augmented PATH.
await copyFile(fileURLToPath(new URL("../runtime/target/debug/treer-acp", import.meta.url)), `${scratch}/bin/treer-acp`);
const runtimeEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("TREER_") && !key.startsWith("TOWER_")));
const runtime = spawn(`${scratch}/bin/treer-acp`, [
  "--fake", "--ui", "builtin", "--cwd", cwd, "--state-dir", `${scratch}/state`,
], { env: { ...runtimeEnv, TREER_AGENT_ID: "browser-fixture", AIS_AUTO_REGISTER: "1" }, stdio: ["ignore", "pipe", "pipe"] });
let logs = "";
runtime.stderr.on("data", (data) => { logs += data; });
let proxy;
let browser;
try {
  const base = await new Promise((resolve, reject) => {
    let output = "";
    runtime.stdout.on("data", (data) => {
      output += data;
      const match = output.match(/listening on (127\.0\.0\.1:\d+)/);
      if (match) resolve(`http://${match[1]}`);
    });
    runtime.once("exit", (code) => reject(new Error(`Runtime exited (${code}): ${logs}`)));
    setTimeout(() => reject(new Error(`Runtime startup timed out: ${logs}`)), 15000).unref();
  });
  await expect.poll(async () => readFile(`${scratch}/registration.txt`, "utf8").catch(() => "")).toContain("--ui-path\n/\n");
  const registration = (await readFile(`${scratch}/registration.txt`, "utf8")).trim().split("\n");
  assert.deepEqual(registration.slice(0, 2), ["interface", "register"]);
  for (const capability of ["prompt.submit", "transcript.read", "state.observe", "abort"]) assert.ok(registration.includes(capability));
  // Mimic the existing tunnel's prefix stripping; the UI must never escape it.
  proxy = createServer(async (request, response) => {
    if (request.url === "/") {
      response.setHeader("Content-Type", "text/html");
      response.end('<iframe title="ACP" src="/tunnel/?theme=dark" style="width:100%;height:900px;border:0"></iframe>');
      return;
    }
    if (!request.url.startsWith("/tunnel/")) { response.writeHead(404).end(); return; }
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const upstream = await fetch(`${base}/${request.url.slice("/tunnel/".length)}`, {
        method: request.method,
        headers: { "Content-Type": request.headers["content-type"] || "application/json" },
        body: chunks.length ? Buffer.concat(chunks) : undefined,
      });
      response.writeHead(upstream.status, { "Content-Type": upstream.headers.get("content-type") });
      response.end(Buffer.from(await upstream.arrayBuffer()));
    } catch (error) { response.writeHead(502).end(JSON.stringify({ error: error.message })); }
  });
  proxy.listen(0, "127.0.0.1");
  await once(proxy, "listening");
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 1100, height: 1000 } });
  const errors = [];
  const requests = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("request", (request) => requests.push({ url: new URL(request.url()), method: request.method() }));
  await page.goto(`http://127.0.0.1:${proxy.address().port}/`);
  const frame = page.frameLocator("iframe");
  await expect(frame.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(frame.locator("#status")).toHaveText("idle");
  const tree = frame.locator("#file-tree");
  await expect(tree.getByRole("button", { name: "▸ src", exact: true })).toBeVisible();
  assert.equal(requests.filter(({ url }) => url.pathname.endsWith("/v1/files/tree") && url.searchParams.get("path") === "src").length, 0);
  await tree.getByRole("button", { name: "▸ src", exact: true }).click();
  await tree.getByRole("button", { name: "· hello & #.txt", exact: true }).click();
  await expect(frame.locator("#file-content")).toContainText("<script>window.injected = true</script>");
  assert.equal(await page.frames()[1].evaluate(() => window.injected), undefined);
  await tree.getByRole("button", { name: "▾ src", exact: true }).click();
  await tree.getByRole("button", { name: "▸ src", exact: true }).click();
  assert.equal(requests.filter(({ url }) => url.pathname.endsWith("/v1/files/tree") && url.searchParams.get("path") === "src").length, 1);
  await tree.getByRole("button", { name: "· large.txt", exact: true }).click();
  await expect(frame.locator("#truncated")).toBeVisible();
  assert.equal((await frame.locator("#file-content").textContent()).length, 65536);
  await page.evaluate(() => document.querySelector("iframe").contentWindow.postMessage({ type: "treer:embed-theme", theme: "light" }, location.origin));
  await expect(frame.locator("html")).toHaveAttribute("data-theme", "light");
  await page.evaluate(() => document.querySelector("iframe").contentWindow.postMessage({ type: "treer:embed-theme", theme: "invalid" }, location.origin));
  await expect(frame.locator("html")).toHaveAttribute("data-theme", "light");
  await frame.locator("#prompt").fill("quick-success");
  await frame.locator("#send").click();
  await expect(frame.locator("#transcript")).toContainText("quick-success");
  await expect(frame.locator("#status")).toHaveText("idle");
  // Simulate a lost response after AIS accepted a prompt; retries reuse the ID.
  const ids = [];
  await page.route("**/v1/prompts", async (route) => {
    ids.push(route.request().postDataJSON().operation_id);
    if (ids.length === 1) { await route.fetch(); await route.abort("failed"); }
    else await route.continue();
  });
  await frame.locator("#prompt").fill("retry-test");
  await frame.locator("#send").click();
  await expect(frame.locator("#prompt-note")).toContainText("Send failed");
  await expect(frame.locator("#send")).toBeEnabled();
  await frame.locator("#send").click();
  await expect(frame.locator("#prompt-note")).toHaveText("Accepted");
  assert.equal(ids.length, 2);
  assert.equal(ids[0], ids[1]);
  await expect(frame.locator('.entry[data-role="user"]').filter({ hasText: "retry-test" })).toHaveCount(1);
  await page.unroute("**/v1/prompts");
  await expect(frame.locator("#status")).toHaveText("idle");
  await frame.locator("#prompt").fill("slow-cancel");
  await frame.locator("#send").click();
  await expect(frame.locator("#abort")).toBeEnabled();
  await frame.locator("#abort").click();
  await expect(frame.locator("#prompt-note")).toHaveText("Abort requested");
  await expect(frame.locator("#status")).toHaveText("idle");
  await frame.locator("#refresh-files").click();
  await expect(tree.getByRole("button", { name: "▸ src", exact: true })).toBeVisible();
  if (process.env.TREER_UI_TEST_SCREENSHOT) await page.screenshot({ path: process.env.TREER_UI_TEST_SCREENSHOT, fullPage: true });
  await page.setViewportSize({ width: 390, height: 850 });
  assert.ok(await page.frames()[1].evaluate(() => document.documentElement.scrollWidth <= innerWidth), "Mobile UI overflows horizontally");
  const allowed = new Set(["/v1/files/tree", "/v1/files", "/v1/transcript", "/v1/prompts", "/v1/status", "/v1/abort"]);
  for (const { url, method } of requests.filter(({ url }) => url.pathname.includes("/v1/"))) {
    assert.ok(url.pathname.startsWith("/tunnel/"), `Escaped tunnel: ${url}`);
    const path = url.pathname.slice("/tunnel".length);
    assert.ok(allowed.has(path), `Unexpected API: ${path}`);
    assert.equal(method, path === "/v1/prompts" || path === "/v1/abort" ? "POST" : "GET");
  }
  assert.deepEqual(errors, []);
  console.log("PASS: UI registration, lazy tree, safe text/encoded paths, truncated preview, embed themes, transcript, prompts, retry idempotency, abort, relative AIS paths");
} finally {
  await browser?.close();
  proxy?.close();
  if (runtime.exitCode == null) {
    const stopped = once(runtime, "exit");
    runtime.kill("SIGINT");
    await stopped;
  }
  await rm(scratch, { recursive: true });
}
