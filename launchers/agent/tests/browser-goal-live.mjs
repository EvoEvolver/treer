// Opt-in: real providers, isolated UI listeners and new temporary sessions only.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, realpath, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium, expect } from "@playwright/test";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";
const root = resolve(import.meta.dirname, "..");
const shots = process.env.AGENT_UI_GOAL_SCREENSHOTS || await mkdtemp(join(tmpdir(), "agent-ui-goal-live-shots-"));
await mkdir(shots, { recursive: true });
const browser = await chromium.launch();
try {
  for (const harness of ["codex", "claude"]) {
    const cwd = await realpath(await mkdtemp(join(tmpdir(), `agent-ui-${harness}-goal-live-`)));
    const portFile = join(cwd, "port");
    const child = spawn(join(root, "node_modules/.bin/tsx"), [join(root, "src/index.ts")], {
      cwd, env: hermeticEnv({ AGENT_UI_INSTANCE_ID: "goal-fixture", AGENT_UI_HARNESS: harness, AGENT_UI_CWD: cwd, AGENT_UI_PORT: "0", AGENT_UI_PORT_FILE: portFile, TREER_AGENT_ID: "ag_goal_live", ...(process.env.CODEX_BIN ? { CODEX_BIN: process.env.CODEX_BIN } : {}) }), stdio: "pipe",
    });
    let logs = ""; child.stderr.on("data", (chunk) => { logs += chunk; });
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, extraHTTPHeaders: { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_goal_live", "x-treer-interface-instance": "goal-fixture" } });
    try {
      let base = "";
      for (let attempt = 0; attempt < 150; attempt++) {
        if (child.exitCode !== null) throw new Error(logs);
        try { base = `http://127.0.0.1:${Number(await readFile(portFile, "utf8"))}`; if ((await fetch(base + "/api/health")).ok) break; } catch { /* startup */ }
        await new Promise((done) => setTimeout(done, 100));
      }
      await page.goto(base + "/?theme=light&look=a");
      await page.getByRole("button", { name: "Set goal", exact: true }).click();
      await page.getByLabel("Objective", { exact: true }).fill("Reply exactly GOAL_DONE and finish this goal. Do not use any tools except the goal-completion tool if available.");
      await page.getByRole("button", { name: "Save goal", exact: true }).click();
      await expect(page.locator("dialog.goal-dialog")).not.toBeVisible();
      const goalSegment = page.getByRole("toolbar", { name: "Run status" }).getByRole("button", { name: /^Goal/ });
      await expect(goalSegment).toContainText("GOAL_DONE");
      await page.screenshot({ path: join(shots, `${harness}-set-desktop.png`) });
      if (harness === "codex") {
        await page.getByRole("textbox", { name: /Message/ }).fill("Complete the current goal now.");
        await page.getByRole("button", { name: "Send message", exact: true }).click();
      }
      await expect(page.locator(".goal-done")).toContainText("Goal complete", { timeout: 60000 }); // the conversation announces the finished goal
      // Codex can announce goal completion before final turn usage is accounted.
      await expect.poll(async () => (await fetch(base + "/api/state").then((response) => response.json())).thread.status, { timeout: 60000 }).toBe("idle");
      let state = await fetch(base + "/api/state").then((response) => response.json());
      assert.equal(state.goal.status, "done");
      assert.ok(typeof state.goal.tokensUsed === "number" && state.goal.tokensUsed >= 0);
      assert.ok(typeof state.goal.timeUsedSeconds === "number" && state.goal.timeUsedSeconds >= 0);
      await page.screenshot({ path: join(shots, `${harness}-complete-desktop.png`) });
      await page.setViewportSize({ width: 390, height: 844 });
      await page.screenshot({ path: join(shots, `${harness}-complete-mobile.png`) });
      await goalSegment.click();
      await page.getByRole("button", { name: "Clear goal", exact: true }).click();
      await page.getByRole("button", { name: "Confirm clear goal", exact: true }).click();
      await expect(page.getByRole("button", { name: "Set goal", exact: true })).toBeVisible({ timeout: 60000 });
      state = await fetch(base + "/api/state").then((response) => response.json());
      assert.equal(state.goal.status, "cleared");
      await page.screenshot({ path: join(shots, `${harness}-cleared-mobile.png`) });
      console.log(JSON.stringify({ harness, result: "passed", cwd, threadId: state.thread.id }));
    } finally {
      await page.close();
      const exited = child.exitCode === null ? new Promise((done) => child.once("exit", done)) : Promise.resolve();
      child.kill("SIGTERM"); await exited;
    }
  }
} finally { await browser.close(); }
console.log(`Real-provider goal screenshots: ${shots}`);
