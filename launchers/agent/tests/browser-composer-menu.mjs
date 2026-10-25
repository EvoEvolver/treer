import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, mkdir, writeFile, readFile, rm, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium, expect } from "@playwright/test";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";
import { testRuntimeControlsUi } from "./browser-controls-ui.mjs";
import { testImagesUi } from "./browser-images-ui.mjs";
import { fakePrivateState } from "./fake-private-state.mjs";
import { backend, spawnLauncher, supports } from "./launcher-backend.mjs";

// The React UI (public/ui.html) against the real launcher server and a fake Codex: no inference, no Treer mutations.
// Usage: node tests/browser-composer-menu.mjs
const app = resolve(import.meta.dirname, "..");
const screenshots = process.env.UI_SCREENSHOTS || process.env.CODEX_UI_SCREENSHOTS || await mkdtemp(join(tmpdir(), "ui-shots-"));
await mkdir(screenshots, { recursive: true });
const servers = [];
const unsupported = (what) => Object.assign(new Error(`${what} is not implemented by the ${backend} launcher`), { unsupported: true });
async function startServer(name, extra, harness = "codex") {
  if (!supports(harness)) throw unsupported(`The ${harness} harness`);
  const cwd = await mkdtemp(join(tmpdir(), `ui-browser-${name}-`));
  const privateRoot = await realpath(await mkdtemp(join(tmpdir(), `ui-images-${name}-`)));
  const controller = await fakePrivateState("ag_browser_ui");
  await mkdir(join(cwd, "src/deep"), { recursive: true });
  await mkdir(join(cwd, "empty"));
  await writeFile(join(cwd, "src/greeting.ts"), "export const greeting = 'Hello, Treer';\n// <script>window.fileXSS = true</script>\n");
  await writeFile(join(cwd, "src/deep/inner.txt"), "inner file\n");
  await writeFile(join(cwd, "README.md"), "# Workspace\nSmall tools, thoughtful interfaces.\n");
  await writeFile(join(cwd, "special # 100%.txt"), "literal percent and hash filename\n");
  await writeFile(join(cwd, "large.txt"), "x".repeat(65537));
  // The unique marker identifies exactly the processes this test started; they are stopped through their own handles only.
  const common = { UI_DEV_FIXTURE: randomUUID(), TREER_AGENT_PRIVATE_STATE_DIR: privateRoot, TREER_AGENT_ID: "ag_browser_ui", TREER_AGENT_SERVER_URL: controller.url, TREER_WORKLOAD_CREDENTIAL: "fixture-only", AGENT_UI_MESSAGE_WATCH: "0", AGENT_UI_INSTANCE_ID: "browser-ui", CODEX_UI_PUBLIC_DIR: join(app, "public"), CODEX_UI_PORT: "0" };
  const env = hermeticEnv(harness === "claude"
    ? { ...common, AGENT_UI_HARNESS: "claude", AGENT_UI_CWD: cwd, AGENT_UI_PORT: "0", AGENT_UI_PORT_FILE: join(cwd, "port"), AGENT_UI_PUBLIC_DIR: join(app, "public"), CLAUDE_BIN: join(app, "tests/fake-claude.mjs"), ...extra }
    : { ...common, CODEX_UI_CWD: cwd, CODEX_UI_PORT_FILE: join(cwd, "port"), CODEX_BIN: join(app, "tests/fake-codex.mjs"), FAKE_CODEX_UX: "1", ...extra });
  const server = spawnLauncher(app, env);
  let logs = "";
  server.stderr.on("data", (chunk) => { logs += chunk; });
  server.stdout.on("data", (chunk) => { logs += chunk; });
  servers.push({ server, cwd, privateRoot, controller });
  let base;
  for (let attempt = 0; attempt < 100; attempt++) {
    if (server.exitCode !== null) throw new Error(logs);
    try {
      base = `http://127.0.0.1:${Number(await readFile(join(cwd, "port"), "utf8"))}`;
      if ((await fetch(base + "/api/health")).ok) break;
    } catch { /* wait for the isolated listener */ }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  assert.ok(base, logs);
  return base;
}
let browser;
let cases = 0;
try {
  const base = await startServer("composer-menu", { FAKE_CODEX_MODELS: "1", FAKE_CODEX_IMAGES: "1" });
  browser = await chromium.launch();
  const context = await browser.newContext({extraHTTPHeaders:{"x-treer-interface-source":"proxy-ui","x-treer-agent-id":"ag_browser_ui","x-treer-interface-instance":"browser-ui"}});
  const page = await context.newPage();
  for (const width of [1280,390]) for (const theme of ["light","dark"]) for (const lang of ["en","zh"]) {
    await page.setViewportSize({width,height:900});
    await page.goto(`${base}/?theme=${theme}&lang=${lang}&look=b`);
    const button = page.getByRole("button",{name:lang==="en"?"Composer actions":"输入选项",exact:true});
    await expect(button).toBeVisible();
    await expect(page.locator(".ctl .model,.ctl .image-add")).toHaveCount(0);
    await expect(page.locator(".strip").getByText(lang==="en"?"Set goal":"设置目标",{exact:true})).toHaveCount(0);
    await button.focus(); await page.keyboard.press("Enter");
    const items=page.getByRole("menuitem"); await expect(items).toHaveCount(3);
    await expect(items.first()).toBeFocused();
    await page.keyboard.press("ArrowDown"); await expect(items.nth(1)).toBeFocused();
    await page.keyboard.press("End"); await expect(items.nth(2)).toBeFocused();
    await page.keyboard.press("Enter"); await expect(page.locator("#model-select")).toBeVisible();
    const back=page.getByRole("button",{name:lang==="en"?"Back to composer actions":"返回输入选项",exact:true});
    await back.click(); await expect(items.first()).toBeFocused();
    await items.nth(1).click(); await expect(page.locator("#goal-objective")).toBeFocused();
    await back.click(); await expect(items.first()).toBeFocused();
    await page.keyboard.press("Escape"); await expect(button).toBeFocused();
    await button.click(); await page.locator("h1").click(); await expect(page.getByRole("menu")).toHaveCount(0);
    await button.click();
    const chooser=page.waitForEvent("filechooser"); await items.first().click();
    await (await chooser).setFiles({name:"menu.png",mimeType:"image/png",buffer:Buffer.from(await page.evaluate(async()=>{const canvas=document.createElement("canvas");canvas.width=2;canvas.height=2;const blob=await new Promise(r=>canvas.toBlob(r,"image/png"));return [...new Uint8Array(await blob.arrayBuffer())];}))});
    await expect(page.locator(".image-drafts img")).toHaveCount(1);
    await page.locator("#ta").fill(`Menu attachment ${width} ${theme} ${lang}`);
    const sent=page.waitForRequest(r=>r.url().endsWith("/api/prompt")&&r.method()==="POST");
    await page.locator("#send").click(); const payload=(await sent).postDataJSON();assert.equal(payload.attachment_ids.length,1);
    await expect(page.locator(".image-drafts img")).toHaveCount(0);
    await button.click();
    for(const item of await items.all()){const r=await item.boundingBox();assert.ok(r.width>=44&&r.height>=44);}
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
    await page.screenshot({path:join(screenshots,`menu-${width}-${theme}-${lang}.png`),animations:"disabled"});
    await page.keyboard.press("Escape");cases++;
  }
  // Existing real model ACK/settings paths, delayed pending state and refusal.
  await page.goto(`${base}/?lang=en`);
  const open=async()=>{await page.getByRole("button",{name:"Composer actions",exact:true}).click();await page.getByRole("menuitem",{name:/Model and effort/}).click();};
  await open();
  let release;const pending=new Promise(r=>release=r);
  await page.route("**/api/settings",async route=>{await pending;await route.fulfill({status:409,json:{error:"Refused fixture settings"}});});
  await page.locator("#model-select").selectOption("gpt-b");await expect(page.locator("#model-select")).toBeDisabled();
  release();await expect(page.getByRole("alert")).toContainText("Refused fixture settings");await expect(page.locator("#model-select")).toBeEnabled();
  await page.unroute("**/api/settings");await page.locator("#model-select").selectOption("gpt-b");await expect(page.locator("#model-select")).toHaveValue("gpt-b");cases++;
  await context.close();
  await testRuntimeControlsUi(browser, screenshots);
  await testImagesUi(browser, base, screenshots);
  console.log(JSON.stringify({passed:cases,screenshots,scope:"local real launcher/fake Codex; no live services"}));
} finally {
  await browser?.close();
  for(const {server,cwd,privateRoot,controller} of servers){const exited=server.exitCode===null?new Promise(r=>server.once("exit",r)):Promise.resolve();server.kill("SIGTERM");await exited;await controller.close();await rm(cwd,{recursive:true,force:true});await rm(privateRoot,{recursive:true,force:true});}
}
