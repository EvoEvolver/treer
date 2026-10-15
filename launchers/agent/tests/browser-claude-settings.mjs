// --fake is an unattended UI regression; without it use a new real Claude session.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, realpath } from "node:fs/promises";
import { tmpdir, homedir } from "node:os";
import { join, resolve } from "node:path";
import { chromium, expect } from "@playwright/test";
import { hermeticEnv, localFetch as fetch } from "./hermetic.mjs";
const fake = process.argv.includes("--fake");
const root = resolve(import.meta.dirname, "..");
const shots = await mkdtemp(join(tmpdir(), "claude-settings-shots-"));
const browser = await chromium.launch();
try {
  for (const unsupported of fake ? [false, true] : [false]) {
    const cwd = await realpath(await mkdtemp(join(tmpdir(), "agent-ui-claude-settings-")));
    const portFile = join(cwd, "port");
    const env = hermeticEnv({ AGENT_UI_INSTANCE_ID: "settings-fixture", AGENT_UI_HARNESS: "claude", AGENT_UI_CWD: cwd, AGENT_UI_PORT: "0", AGENT_UI_PORT_FILE: portFile, TREER_AGENT_ID: "ag_settings_test", CLAUDE_MODEL: "sonnet",
      ...(fake ? { CLAUDE_BIN: join(root, "tests/fake-claude.mjs"), FAKE_CLAUDE_RESOLVED_MODELS: "1", FAKE_CLAUDE_SLOW_SETTINGS: "1", ...(unsupported ? { FAKE_CLAUDE_NO_EFFORT: "1" } : {}) } : {}) });
    const server = spawn(join(root, "node_modules/.bin/tsx"), [join(root, "src/index.ts")], { cwd, env, stdio: "pipe" });
    let logs = ""; server.stderr.on("data", (chunk) => { logs += chunk; });
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, extraHTTPHeaders: { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "ag_settings_test", "x-treer-interface-instance": "settings-fixture" } });
    try {
      let base = "";
      for (let attempt = 0; attempt < 150; attempt++) {
        if (server.exitCode !== null) throw new Error(logs);
        try { base = `http://127.0.0.1:${Number(await readFile(portFile, "utf8"))}`; if ((await fetch(base + "/api/health")).ok) break; } catch { /* startup */ }
        await new Promise((done) => setTimeout(done, 100));
      }
      const state = () => fetch(base + "/api/state").then((r) => r.json());
      const post = (path, body) => fetch(base + path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }).then(async (r) => { assert.ok(r.ok, await r.clone().text()); return r.json(); });
      await page.goto(base + "/?theme=light&look=a");
      let before = await state();
      const firstId = before.thread.id;
      // Model and effort live in the model popover opened from the composer chip.
      await page.locator(".model").click();
      await expect(page.locator("#model-select")).toHaveValue(before.thread.model);
      assert.ok(await page.locator("#model-select option:checked").textContent());
      if (unsupported) await expect(page.locator("#effort-select")).toHaveCount(0);
      else await expect(page.locator("#effort-select")).toBeVisible();
      await page.screenshot({ path: join(shots, `${unsupported ? "unsupported" : "supported"}-initial-desktop.png`) });
      if (fake) await post("/api/prompt", { prompt: "work" });
      else {
        await post("/api/prompt", { prompt: "Protocol test. Call Bash foreground exactly once: sleep 4; echo FIRST_DONE. Then reply FIRST_DONE. Do not read files or use other tools." });
        await expect.poll(async () => (await state()).thread.turns.at(-1)?.items.some((item) => item.kind === "commandExecution" && item.status === "inProgress"), { timeout: 45000 }).toBe(true);
      }
      before = await state();
      const target = before.modelOptions.find((option) => option.id === (fake ? "opus" : "haiku"));
      assert.ok(target);
      const changed = page.waitForResponse((r) => r.url().endsWith("/api/settings") && r.request().postDataJSON()?.model === target.model);
      await page.locator("#model-select").selectOption(target.model);
      // Submitting during a switch must retain the message, not restart or drop it.
      await post("/api/prompt", { prompt: "Reply exactly SECOND_DONE. No tools.", mode: "queue" });
      assert.ok((await changed).ok());
      await expect.poll(async () => { const s = await state(); return s.thread.status === "idle" && !s.queue.length && s.thread.turns.some((turn) => turn.items.some((item) => item.kind === "userMessage" && item.text.includes("SECOND_DONE"))); }, { timeout: 60000 }).toBe(true);
      const after = await state();
      assert.equal(after.thread.id, firstId);
      assert.equal(after.thread.model, target.model);
      assert.equal(after.queue.length, 0);
      assert.ok(after.thread.turns.some((turn) => turn.items.some((item) => item.kind === "userMessage" && item.text.includes("SECOND_DONE"))));
      if (!fake) {
        assert.ok(after.thread.turns.some((turn) => turn.items.some((item) => item.kind === "agentMessage" && item.text === "SECOND_DONE")));
        const historyFile = join(process.env.CLAUDE_CONFIG_DIR || join(homedir(), ".claude"), "projects", cwd.replace(/[^a-zA-Z0-9]/g, "-"), `${firstId}.jsonl`);
        const entries = (await readFile(historyFile, "utf8")).trim().split("\n").map((line) => JSON.parse(line));
        const reply = entries.findLast((e) => e.type === "assistant" && e.message?.content?.some((b) => b.type === "text" && b.text.includes("SECOND_DONE")));
        assert.equal(reply?.message.model, target.model, "Native history must confirm the new actual model");
      }
      if (!unsupported) {
        const changedEffort = page.waitForResponse((r) => r.url().endsWith("/api/settings") && r.request().postDataJSON()?.reasoningEffort === "low");
        await page.locator("#effort-select").selectOption("low");
        assert.ok((await changedEffort).ok());
        await expect(page.locator("#effort-select")).toHaveValue("low");
        if (!fake) {
          await post("/api/prompt", { prompt: "/model" });
          await expect.poll(async () => (await state()).thread.status, { timeout: 30000 }).toBe("idle");
          assert.ok((await state()).thread.turns.at(-1).items.some((item) => item.text.includes("effort: low")), "Native /model must confirm live effort");
        }
      }
      await expect(page.locator("#model-select")).toHaveValue(target.model);
      await page.screenshot({ path: join(shots, `${unsupported ? "unsupported" : "supported"}-switched-desktop.png`) });
      await page.setViewportSize({ width: 390, height: 844 });
      if (unsupported) await expect(page.locator("#effort-select")).toHaveCount(0);
      else await expect(page.locator("#effort-select")).toBeVisible();
      await page.screenshot({ path: join(shots, `${unsupported ? "unsupported" : "supported"}-switched-mobile.png`) });
      assert.ok(await page.evaluate(() => document.querySelector(".top").scrollWidth <= innerWidth && document.documentElement.scrollWidth <= innerWidth), "Header controls must fit the narrow viewport");
      console.log(JSON.stringify({ fake, unsupported, cwd, sessionId: firstId, actualModel: target.model, result: "passed" }));
    } finally {
      await page.close();
      const exited = server.exitCode === null ? new Promise((done) => server.once("exit", done)) : Promise.resolve();
      server.kill("SIGTERM"); await exited;
    }
  }
} finally { await browser.close(); }
console.log(`Claude model/effort screenshots: ${shots}`);
