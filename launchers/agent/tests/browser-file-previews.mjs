// Production build, real AIS backend, synthetic workspace/provider, no live
// credentials or managed processes. A prefix relay models Proxy path rewriting.
import assert from "node:assert/strict";
import { createServer, request as httpRequest } from "node:http";
import { connect } from "node:net";
import { mkdtemp, mkdir, writeFile, readFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { once } from "node:events";
import { chromium, expect } from "@playwright/test";
import { hermeticEnv, localFetch } from "./hermetic.mjs";
import { fakePrivateState } from "./fake-private-state.mjs";
import { backend, spawnLauncher } from "./launcher-backend.mjs";

const app = resolve(import.meta.dirname, "..");
const root = await realpath(await mkdtemp(join(tmpdir(), "treer-file-previews-")));
const cwd = join(root, "workspace"), state = join(root, "state"), port = join(root, "port");
await mkdir(cwd); await mkdir(state, { mode: 0o700 });
// Small valid two-page PDF with generated xref offsets (no third-party fixture).
const objects = [
  "<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>",
  "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 250] /Contents 5 0 R >>",
  "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 250] /Contents 6 0 R >>",
  "<< /Length 24 >>\nstream\n0 0 1 rg 10 10 50 50 re f\nendstream",
  "<< /Length 24 >>\nstream\n1 0 0 rg 20 20 60 60 re f\nendstream",
];
let pdf = "%PDF-1.7\n", offsets = [0];
objects.forEach((body, i) => { offsets.push(Buffer.byteLength(pdf)); pdf += `${i + 1} 0 obj\n${body}\nendobj\n`; });
const xref = Buffer.byteLength(pdf);
pdf += `xref\n0 7\n0000000000 65535 f \n${offsets.slice(1).map(n => String(n).padStart(10, "0") + " 00000 n \n").join("")}trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
await writeFile(join(cwd, "two.pdf"), pdf);
await writeFile(join(cwd, "broken.pdf"), "not a PDF");
await writeFile(join(cwd, "plain.txt"), "plain source only");
await writeFile(join(cwd, "guide.md"), '# Lazy Markdown\n**safe bold**\n<script>window.previewXss=1</script>\n[x](javascript:alert(1))');
await writeFile(join(cwd, "pixel.png"), Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=", "base64"));
await writeFile(join(cwd, "slow.png"), await readFile(join(cwd, "pixel.png")));
await writeFile(join(cwd, "oversized.pdf"), Buffer.alloc(20 * 1024 * 1024 + 1));
const controller = await fakePrivateState("ag_preview_fixture");
const env = hermeticEnv({ TREER_AGENT_ID: "ag_preview_fixture", TREER_AGENT_SERVER_URL: controller.url, TREER_WORKLOAD_CREDENTIAL: "fixture-only", TREER_AGENT_PRIVATE_STATE_DIR: state,
  AGENT_UI_CWD: cwd, AGENT_UI_PUBLIC_DIR: process.env.PREVIEW_PUBLIC_DIR || join(app, "public"), AGENT_UI_PORT: "0", AGENT_UI_PORT_FILE: port, AGENT_UI_INSTANCE_ID: "preview-fixture", AGENT_UI_MESSAGE_WATCH: "0", RUST_SPIKE_REGISTER: "0", CODEX_BIN: join(app, "tests/fake-codex.mjs") });
const child = spawnLauncher(app, env);
let logs = "", browser, relay;
child.stderr.on("data", b => { logs = (logs + b).slice(-8192); }); child.stdout.resume();
try {
  let base;
  for (let n = 0; n < 200; n++) {
    if (child.exitCode !== null) throw Error(logs);
    try { const p = Number(await readFile(port, "utf8")); base = `http://127.0.0.1:${p}`; if ((await localFetch(base + "/api/health")).ok) break; } catch {}
    await new Promise(r => setTimeout(r, 30));
  }
  assert.ok(base, logs);
  const prefix = "/proxy/agent/interface/";
  const served = [];
  let cancelledRead = false;
  relay = createServer((req, res) => {
    assert.ok(req.url.startsWith(prefix));
    const path = req.url.slice(prefix.length);
    served.push(path);
    if (path.startsWith("v1/files/download?") && path.includes("slow.png")) {
      const timer = setTimeout(() => res.end("late old bytes"), 3000);
      res.once("close", () => { cancelledRead = true; clearTimeout(timer); });
      return;
    }
    const upstream = httpRequest(base + "/" + path, { method: req.method, headers: { ...req.headers, host: new URL(base).host } }, response => { res.writeHead(response.statusCode, response.headers); response.pipe(res); });
    upstream.on("error", () => res.destroy()); req.pipe(upstream); res.on("close", () => upstream.destroy());
  });
  relay.on("upgrade", (req, socket, head) => {
    const upstream = connect(Number(new URL(base).port), "127.0.0.1", () => {
      upstream.write(`GET /${req.url.slice(prefix.length)} HTTP/1.1\r\n${Object.entries({ ...req.headers, host: new URL(base).host }).map(([k,v]) => `${k}: ${v}`).join("\r\n")}\r\n\r\n`);
      if (head.length) upstream.write(head); socket.pipe(upstream).pipe(socket);
    });
    upstream.on("error", () => socket.destroy()); socket.on("error", () => upstream.destroy()); socket.on("close", () => upstream.destroy());
  });
  await new Promise(r => relay.listen(0, "127.0.0.1", r));
  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, extraHTTPHeaders: {
    "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_preview_fixture", "x-treer-interface-instance": "preview-fixture" } });
  const page = await context.newPage();
  const errors = [], violations = [];
  page.on("pageerror", e => errors.push(e.message));
  await page.addInitScript(() => {
    window.__previewViolations = []; window.__previewURLs = new Set();
    document.addEventListener("securitypolicyviolation", e => window.__previewViolations.push(e.violatedDirective));
    const create = URL.createObjectURL.bind(URL), revoke = URL.revokeObjectURL.bind(URL);
    URL.createObjectURL = blob => { const url = create(blob); window.__previewURLs.add(url); return url; };
    URL.revokeObjectURL = url => { window.__previewURLs.delete(url); revoke(url); };
  });
  const resources = () => served.filter(path => /\.js(?:\?|$)/.test(path));
  const rich = () => resources().filter(path => /(?:MarkdownPreview|ImagePreview|PdfPreview|pdf-worker)-/.test(path));
  await page.goto(`http://127.0.0.1:${relay.address().port}${prefix}?lang=en`);
  await expect(page.getByRole("button", { name: "Browse files", exact: true })).toBeVisible();
  assert.deepEqual(rich(), [], "cold chat must not fetch rich viewers");
  await page.getByRole("button", { name: "Browse files", exact: true }).click();
  await expect(page.getByRole("treeitem", { name: "plain.txt", exact: true })).toBeVisible();
  assert.deepEqual(rich(), [], "directory listing must not fetch rich viewers");
  const choose = name => page.getByRole("treeitem", { name, exact: true }).click();
  await choose("plain.txt"); await expect(page.locator(".preview pre")).toHaveText("plain source only");
  assert.deepEqual(rich(), [], "plain text must not fetch rich viewers");
  await choose("guide.md"); await expect(page.locator(".file-markdown h1")).toHaveText("Lazy Markdown");
  if (process.env.UI_SCREENSHOTS) await page.screenshot({ path: join(process.env.UI_SCREENSHOTS, `file-markdown-${backend}-desktop-light.png`) });
  assert.equal(rich().filter(p => p.startsWith("MarkdownPreview-")).length, 1);
  assert.equal(rich().filter(p => /ImagePreview|PdfPreview|pdf-worker/.test(p)).length, 0);
  assert.equal(await page.evaluate(() => window.previewXss), undefined);
  await page.getByRole("button", { name: "Source", exact: true }).click(); await expect(page.locator(".preview pre")).toContainText("# Lazy Markdown");
  await page.getByRole("button", { name: "Rendered", exact: true }).click(); await expect(page.locator(".file-markdown h1")).toBeVisible();
  await choose("pixel.png"); await expect(page.locator(".file-image img")).toBeVisible();
  assert.match(await page.locator(".file-image img").getAttribute("src"), /^blob:/);
  await page.getByRole("button", { name: "Actual size", exact: true }).click(); await expect(page.locator(".image-scroll")).toHaveClass(/actual/);
  assert.equal(rich().filter(p => /PdfPreview|pdf-worker/.test(p)).length, 0, "image must not load PDF engine or worker");
  await choose("slow.png");
  await expect.poll(() => served.some(p => p.includes("download?") && p.includes("slow.png"))).toBe(true);
  await choose("plain.txt"); await expect(page.locator(".preview pre")).toHaveText("plain source only");
  await expect.poll(() => cancelledRead).toBe(true);
  assert.equal(await page.evaluate(() => window.__previewURLs.size), 0, "image object URLs must be revoked on selection change");
  await choose("two.pdf"); await expect(page.locator(".file-pdf canvas")).toBeVisible();
  await expect(page.locator(".file-pdf [role=status]")).toHaveCount(0);
  await expect(page.locator(".file-pdf")).toContainText("Page 1 / 2");
  assert.equal(page.workers().length, 1, "PDF must render with a real worker");
  await expect.poll(() => page.locator(".file-pdf canvas").evaluate(canvas => [...canvas.getContext("2d").getImageData(20, canvas.height - 20, 1, 1).data])).toEqual([0, 0, 255, 255]);
  if (process.env.UI_SCREENSHOTS) await page.screenshot({ path: join(process.env.UI_SCREENSHOTS, `file-pdf-${backend}-desktop-light.png`) });
  assert.equal(rich().filter(p => p.startsWith("PdfPreview-")).length, 1);
  assert.equal(rich().filter(p => p.startsWith("pdf-worker-")).length, 1);
  await page.getByRole("button", { name: "Next", exact: true }).click(); await expect(page.locator(".file-pdf")).toContainText("Page 2 / 2");
  await page.getByRole("button", { name: "Zoom in", exact: true }).click(); await expect(page.locator(".file-pdf")).toContainText("125%");
  await expect(page.locator(".file-pdf [role=status]")).toHaveCount(0);
  const pdfBefore = rich().filter(p => /PdfPreview|pdf-worker/.test(p));
  await choose("plain.txt"); await expect(page.locator(".preview pre")).toBeVisible();
  await expect.poll(() => page.workers().length).toBe(0);
  await choose("two.pdf"); await expect(page.locator(".file-pdf [role=status]")).toHaveCount(0); await expect(page.locator(".file-pdf")).toContainText("Page 1 / 2");
  const pdfAfter = rich().filter(p => /PdfPreview|pdf-worker/.test(p));
  // Modules use the module map even under no-store. Hashed-worker HTTP cache
  // is asserted by the current backend only when its header supports it.
  assert.equal(pdfAfter.filter(p => p.startsWith("PdfPreview-")).length, 1);
  if (process.env.EXPECT_WORKER_CACHE === "1") assert.deepEqual(pdfAfter, pdfBefore, "reopened worker must use HTTP cache");
  await choose("oversized.pdf"); await expect(page.locator(".preview [role=alert]")).toContainText("20 MiB");
  assert.ok(!served.some(p => p.startsWith("v1/files/download?") && p.includes("oversized.pdf")));
  await choose("broken.pdf"); await expect(page.locator(".file-pdf [role=alert]")).toContainText("Unable to preview");
  await page.getByRole("button", { name: "Close file browser", exact: true }).click();
  await expect(page.locator(".file-pdf")).toHaveCount(0);
  await expect.poll(() => page.workers().length).toBe(0);
  // Mobile, translated controls and both themes use the same production UI.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`http://127.0.0.1:${relay.address().port}${prefix}?lang=zh&theme=dark`);
  await page.getByRole("button", { name: "菜单", exact: true }).click();
  await page.getByRole("menuitem", { name: "文件", exact: true }).click();
  await choose("two.pdf"); await expect(page.locator(".file-pdf")).toContainText("第 1 / 2 页");
  await expect(page.locator(".file-pdf [role=status]")).toHaveCount(0);
  assert.ok((await page.locator(".pdf-scroll").boundingBox()).height > 300, "mobile viewer must use remaining screen height");
  for (const button of await page.locator(".preview-tools button:visible").all()) {
    const rect = await button.boundingBox(); assert.ok(rect.width >= 44 && rect.height >= 44);
  }
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "phone must not overflow horizontally");
  if (process.env.UI_SCREENSHOTS) await page.screenshot({ path: join(process.env.UI_SCREENSHOTS, `file-pdf-${backend}-phone-dark.png`) });
  await page.getByRole("button", { name: "下一页", exact: true }).focus();
  await page.keyboard.press("Enter"); await expect(page.locator(".file-pdf")).toContainText("第 2 / 2 页");
  violations.push(...await page.evaluate(() => window.__previewViolations));
  assert.deepEqual(errors, []); assert.deepEqual(violations, []);
  console.log(JSON.stringify({ backend, resources: resources(), pdfBefore, pdfAfter, cancelledRead, assertions: "cold/list/text0 rich; md-only; image-noPDF; PDF worker/pages/zoom/reopen/error/bound/disposal; mobile390 dark zh; prefix actual AIS", errors, violations }, null, 2));
  await browser.close(); browser = null;
} finally {
  if (browser) await browser.close();
  if (child.exitCode === null) { const exited = once(child, "exit"); child.kill(); await exited; }
  if (relay) { relay.closeAllConnections(); await new Promise(r => relay.close(r)); }
  await controller.close(); await rm(root, { recursive: true, force: true });
}
