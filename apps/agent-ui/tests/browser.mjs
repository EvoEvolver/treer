import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium, expect } from "@playwright/test";
import { hermeticEnv } from "./hermetic.mjs";

// An isolated listener and fake Codex: no inference, Treer mutations or Agent restart.
const app = resolve(import.meta.dirname, "..");
const cwd = await mkdtemp(join(tmpdir(), "codex-ui-browser-"));
const screenshots = process.env.CODEX_UI_SCREENSHOTS || await mkdtemp(join(tmpdir(), "codex-ui-shots-"));
await mkdir(screenshots, { recursive: true });
await mkdir(join(cwd, "src"));
await writeFile(join(cwd, "src/greeting.ts"), "export const greeting = 'Hello, Treer';\n// <script>window.fileXSS = true</script>\n");
await writeFile(join(cwd, "README.md"), "# Workspace\nSmall tools, thoughtful interfaces.\n");
await writeFile(join(cwd, "large.txt"), "x".repeat(65537));
const env = hermeticEnv({ CODEX_UI_CWD: cwd, CODEX_UI_PUBLIC_DIR: join(app, "public"), CODEX_UI_PORT: "0", CODEX_UI_PORT_FILE: join(cwd, "port"),
  CODEX_BIN: join(app, "tests/fake-codex.mjs"), FAKE_CODEX_UX: "1" });
const server = spawn(join(app, "node_modules/.bin/tsx"), [join(app, "src/index.ts")], { env, stdio: "pipe" });
let logs = "";
server.stderr.on("data", (chunk) => { logs += chunk; });
server.stdout.on("data", (chunk) => { logs += chunk; });
let browser;
try {
  let base;
  for (let attempt = 0; attempt < 100; attempt++) {
    if (server.exitCode !== null) throw new Error(logs);
    try {
      base = `http://127.0.0.1:${Number(await readFile(join(cwd, "port"), "utf8"))}`;
      if ((await fetch(base + "/api/health")).ok) break;
    } catch { /* wait for isolated listener */ }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.ok(base, logs);
  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, colorScheme: "dark" });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => { errors.push(error.message); console.error(error.message); });
  const fileRequests = [];
  page.on("request", (request) => { if (request.url().includes("/v1/files")) fileRequests.push(request.url()); });
  await page.goto(base + "/?theme=light");
  await expect(page.locator(".plan-steps li")).toHaveCount(3);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  assert.equal(fileRequests.length, 0, "File tree should load only on demand");
  const changes = page.locator("details").filter({ hasText: "File change" });
  await changes.locator("summary").click();
  await expect(changes.locator(".diff-path")).toHaveCount(2);
  await expect(changes.locator(".diff-add").first()).toContainText("Hello, Treer");
  await expect(changes.locator(".diff-remove")).toContainText("Hello");
  await page.getByRole("button", { name: "Browse files", exact: true }).click();
  await page.getByRole("button", { name: "src", exact: true }).click();
  await page.getByRole("button", { name: "greeting.ts", exact: true }).click();
  await expect(page.locator("#file-content")).toContainText("<script>window.fileXSS = true</script>");
  assert.equal(await page.evaluate(() => window.fileXSS), undefined);
  await page.screenshot({ path: join(screenshots, "desktop-light.png") });
  await page.evaluate(() => window.postMessage({ type: "treer:embed-theme", theme: "dark" }, "*"));
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.screenshot({ path: join(screenshots, "desktop-dark.png") });
  // State updates must not collapse an expanded diff card.
  await page.getByRole("combobox", { name: "Reasoning effort" }).selectOption("high");
  await expect(changes).toHaveAttribute("open", "");
  await page.getByRole("button", { name: "large.txt", exact: true }).click();
  await expect(page.locator("#file-meta")).toContainText("Truncated to 64 KiB");
  assert.equal((await page.locator("#file-content").textContent()).length, 65536);
  await page.getByRole("button", { name: "greeting.ts", exact: true }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.locator("#file-content")).toBeVisible();
  await page.screenshot({ path: join(screenshots, "mobile-preview.png") });
  await page.getByRole("button", { name: "Browse", exact: true }).click();
  await expect(page.locator("#file-tree")).toBeVisible();
  await expect(page.locator("#file-content")).not.toBeVisible();
  await page.screenshot({ path: join(screenshots, "mobile-files.png") });
  await page.getByRole("button", { name: "Close file browser" }).click();
  await expect(page.locator("#timeline")).toBeVisible();
  await page.screenshot({ path: join(screenshots, "mobile-chat.png") });
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), "No horizontal viewport overflow");
  await page.goto(base + "/");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  // Real parent window messages, including a dynamically changed theme.
  await page.setContent(`<iframe title="Agent" src="${base}/?theme=light" style="width:100%;height:700px"></iframe>`);
  const frame = page.frameLocator("iframe");
  await expect(frame.locator("html")).toHaveAttribute("data-theme", "light");
  await page.evaluate(() => document.querySelector("iframe").contentWindow.postMessage({ type: "treer:embed-theme", theme: "dark" }, "*"));
  await expect(frame.locator("html")).toHaveAttribute("data-theme", "dark");
  assert.deepEqual(errors, []);
  console.log(`Browser checks passed (desktop 1440px / mobile 390px). Screenshots: ${screenshots}`);
} finally {
  await browser?.close();
  const exited = server.exitCode === null ? new Promise((resolve) => server.once("exit", resolve)) : Promise.resolve();
  server.kill("SIGTERM");
  await exited;
  await rm(cwd, { recursive: true, force: true });
}
