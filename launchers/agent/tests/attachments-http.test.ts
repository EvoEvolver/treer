import assert from "node:assert/strict";
import { test } from "node:test";
import { spawn } from "node:child_process";
import { mkdtempSync, realpathSync, rmSync, readFileSync, mkdirSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import { hermeticEnv, localFetch } from "./hermetic.mjs";
import { fakePrivateState } from "./fake-private-state.mjs";

const GIF = Buffer.from("R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==", "base64");
test("attachment HTTP routes retain guards, expose only IDs and fail closed without private state", async t => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "attachments-http-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const app = resolve(import.meta.dirname, "..");
  for (const mode of ["missing", "available", "home", "unsafe", "inside", "vcs", "old", "administrator"] as const) {
    const enabled = mode === "available" || mode === "home";
    const controller = await fakePrivateState("ag_image_http", "fixture-only", { status: mode === "administrator" ? "windows_requires_administrator" : mode === "unsafe" ? "agent_private_state_unsafe" : mode === "old" ? "old-unsupported" : "available" });
    const cwd = join(root, `cwd-${mode}`), privateRoot = mode === "home" ? join(cwd, ".state", "agent") : join(root, `private-${mode}`), portFile = join(root, `port-${mode}`);
    mkdirSync(cwd); mkdirSync(privateRoot, { recursive: true });
    if (mode === "vcs") mkdirSync(join(privateRoot, ".git"));
    const options = { cwd: app, stdio: "pipe" as const, env: hermeticEnv({ AGENT_UI_CWD: cwd, AGENT_UI_PORT_FILE: portFile, CODEX_BIN: join(app, "tests/fake-codex.mjs"), FAKE_CODEX_IMAGES: "1", TREER_AGENT_ID: "ag_image_http", TREER_AGENT_SERVER_URL: controller.url, TREER_WORKLOAD_CREDENTIAL: "fixture-only", AGENT_UI_MESSAGE_WATCH: "0", ...(mode !== "missing" ? { TREER_AGENT_PRIVATE_STATE_DIR: mode === "inside" ? cwd : privateRoot } : {}) }) };
    let child = spawn(process.execPath, ["--import", "tsx", join(app, "src/index.ts")], options);
    let log = ""; child.stderr!.on("data", bytes => { log += bytes; });
    try {
      let base = "";
      const deadline = Date.now() + 10000;
      while (Date.now() < deadline) {
        try { base = `http://127.0.0.1:${readFileSync(portFile, "utf8")}`; if ((await localFetch(`${base}/api/health`)).ok) break; } catch { /* isolated listener starting */ }
        if (child.exitCode !== null) throw new Error(log);
        await new Promise(resolve => setTimeout(resolve, 20));
      }
      assert.ok(base);
      const storageState = await (await localFetch(`${base}/api/state`)).json();
      assert.equal(storageState.attachmentsAvailable, enabled);
      assert.equal(storageState.attachmentStorageStatus, enabled ? "available" : mode === "administrator" ? "windows_requires_administrator" : mode === "inside" ? "agent_private_state_inside_cwd" : mode === "vcs" ? "agent_private_state_inside_vcs_worktree" : mode === "unsafe" ? "agent_private_state_unsafe" : "agent_private_state_unavailable");
      assert.ok(!JSON.stringify(storageState).includes(privateRoot), "private filesystem root never enters browser state");
      // localFetch uses node:http (and accepts Buffer); its shared declaration
      // has RequestInit's browser BodyInit spelling.
      const post = (body: Buffer, headers = {}) => localFetch(`${base}/api/attachments`, { method: "POST", body: body as unknown as BodyInit, headers });
      assert.equal((await post(GIF, { origin: "https://attacker.invalid" })).status, 403);
      const upload = await post(GIF);
      assert.equal(upload.status, enabled ? 201 : 503);
      if (mode === "administrator") {
        assert.deepEqual(readdirSync(privateRoot), [], "no attachment fallback or private file writes");
        assert.equal(controller.events.length, 1, "disabled storage never requests a reservation");
        const text = await localFetch(`${base}/api/prompt`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ prompt: "text-only recovery", mode: "queue" }) });
        assert.equal(text.status, 200);
      }
      if (!enabled) continue;
      const { attachment } = await upload.json();
      assert.deepEqual(Object.keys(attachment).sort(), ["id", "mime", "size"]);
      const get = await localFetch(`${base}/api/attachments/${attachment.id}`);
      assert.equal(get.status, 200); assert.equal(get.headers.get("content-type"), "image/gif");
      assert.equal(get.headers.get("x-content-type-options"), "nosniff");
      assert.equal(get.headers.get("content-security-policy"), "default-src 'none'; sandbox");
      assert.equal(Buffer.from(await get.arrayBuffer()).equals(GIF), true);
      assert.equal((await localFetch(`${base}/api/attachments/${attachment.id}`, { headers: { "x-treer-interface-source": "proxy-ui", "x-treer-agent-id": "different-agent", "x-treer-interface-instance": "other" } })).status, 403);
      assert.equal((await localFetch(`${base}/api/attachments/img_${"0".repeat(32)}`)).status, 404);
      assert.equal((await post(Buffer.from("MZexe"), { "content-type": "image/png" })).status, 400);
      assert.equal((await localFetch(`${base}/api/attachments/${attachment.id}`, { method: "DELETE" })).status, 200);
      assert.equal((await localFetch(`${base}/api/attachments/${attachment.id}`)).status, 404);
      const retained = (await (await post(GIF)).json()).attachment;
      const sent = await localFetch(`${base}/api/prompt`, { method: "POST", body: JSON.stringify({ prompt: "remember this image", attachment_ids: [retained.id], mode: "queue" }), headers: { "content-type": "application/json" } });
      assert.equal(sent.status, 200);
      const recordedDeadline = Date.now() + 5000;
      let recorded = false;
      while (Date.now() < recordedDeadline) {
        const state = await (await localFetch(`${base}/api/state`)).json();
        recorded = state.thread?.turns.some((turn: { items: { attachments?: { id: string }[] }[] }) => turn.items.some(item => item.attachments?.some(image => image.id === retained.id)));
        if (recorded) break;
        await new Promise(resolve => setTimeout(resolve, 20));
      }
      assert.equal(recorded, true, "native completed user item is durably bound before restart");
      const stopped = new Promise(resolve => child.once("exit", resolve)); child.kill("SIGTERM"); await stopped;
      child = spawn(process.execPath, ["--import", "tsx", join(app, "src/index.ts")], options);
      child.stderr!.on("data", bytes => { log += bytes; });
      let restored = false;
      const resumeDeadline = Date.now() + 10000;
      while (Date.now() < resumeDeadline) {
        try {
          base = `http://127.0.0.1:${readFileSync(portFile, "utf8")}`;
          const state = await (await localFetch(`${base}/api/state`)).json();
          restored = state.thread?.turns.some((turn: { items: { attachments?: { id: string }[] }[] }) => turn.items.some(item => item.attachments?.some(image => image.id === retained.id)));
          if (restored) break;
        } catch { /* old listener stopped, resumed listener not ready */ }
        await new Promise(resolve => setTimeout(resolve, 20));
      }
      assert.equal(restored, true, log);
      assert.equal((await localFetch(`${base}/api/attachments/${retained.id}`)).status, 200);
      // Keep the fake Controller available to prove the launcher's independent
      // mirror checks again after startup, rather than relying on Core refusal.
      const eventsBefore = controller.events.length;
      const filesBefore = readdirSync(join(privateRoot, "images"));
      const marker = join(privateRoot, ".git"); mkdirSync(marker);
      const changed = await (await localFetch(`${base}/api/state`)).json();
      assert.equal(changed.attachmentsAvailable, false);
      assert.equal(changed.attachmentStorageStatus, "agent_private_state_inside_vcs_worktree");
      assert.ok(!JSON.stringify(changed).includes(privateRoot));
      const refused = await post(GIF);
      assert.equal(refused.status, 503);
      assert.equal(controller.events.length, eventsBefore, "mirror refuses before Core reservation");
      assert.deepEqual(readdirSync(join(privateRoot, "images")), filesBefore, "no files created during refused upload");
      rmSync(marker, { recursive: true });
      assert.equal((await (await localFetch(`${base}/api/state`)).json()).attachmentsAvailable, true, "recovery is not cached");
    } finally {
      const stopped = child.exitCode === null ? new Promise(resolve => child.once("exit", resolve)) : Promise.resolve();
      child.kill("SIGTERM"); await stopped;
      await controller.close();
    }
  }
});
