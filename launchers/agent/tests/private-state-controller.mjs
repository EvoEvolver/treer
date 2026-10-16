// Manual joint check: ONLY the ignored Core private_state_joint_fixture output.
// Usage: node tests/private-state-controller.mjs <fixture-log>
// The log contains a synthetic credential; never point this at a real enrollment.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { request } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { hermeticEnv } from "./hermetic.mjs";

const fixture = readFileSync(process.argv[2], "utf8").split("\n").flatMap(line => {
  try { const value = JSON.parse(line); return value.fixture === "private_state_joint_fixture" ? [value] : []; } catch { return []; }
})[0];
assert.ok(fixture, "expected the synthetic Core joint fixture handshake");
assert.equal(fixture.agent_id, "ag_fixture");
assert.equal(fixture.server_id, "srv_fixture");
assert.equal(fixture.feature, "agent.private-state.v1");
assert.ok(fixture.delete_after_seconds > 0, "fixture must schedule confirmed deletion");
const app = resolve(import.meta.dirname, "..");
const cwd = mkdtempSync(join(tmpdir(), "launcher-core-joint-"));
const portFile = join(cwd, "port");
const gif = Buffer.from("R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==", "base64");
function http(url, method = "GET", body, headers = {}) {
  return new Promise((resolve, reject) => {
    const req = request(url, { method, headers, signal: AbortSignal.timeout(5000) }, res => {
      const chunks = []; res.on("data", chunk => chunks.push(chunk)); res.on("error", reject);
      res.on("end", () => resolve({ status: res.statusCode, bytes: Buffer.concat(chunks), json() { return JSON.parse(this.bytes); } }));
    });
    req.on("error", reject); req.end(body);
  });
}
const identity = { "x-treer-agent-id": fixture.agent_id, "x-treer-workload-credential": fixture.workload_credential };
const core = (path, method, body) => http(`${fixture.url}/api/private-state${path}`, method, body === undefined ? undefined : JSON.stringify(body), { ...identity, "content-type": "application/json" });
let child;
async function stop() {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, "exit"); child.kill("SIGTERM"); await exited;
}
async function start() {
  rmSync(portFile, { force: true });
  child = spawn(process.execPath, ["--import", "tsx", "src/index.ts"], {
    cwd: app, stdio: "ignore", env: hermeticEnv({ AGENT_UI_CWD: cwd, AGENT_UI_PORT_FILE: portFile,
      AGENT_UI_MESSAGE_WATCH: "0", CODEX_BIN: join(app, "tests/fake-codex.mjs"), FAKE_CODEX_IMAGES: "1",
      TREER_AGENT_SERVER_URL: fixture.url, TREER_AGENT_ID: fixture.agent_id,
      TREER_WORKLOAD_CREDENTIAL: fixture.workload_credential, TREER_AGENT_PRIVATE_STATE_DIR: fixture.private_state_dir }),
  });
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) {
    assert.equal(child.exitCode, null, "synthetic launcher exited during startup");
    try {
      const port = readFileSync(portFile, "utf8").trim();
      if (/^[1-9]\d*$/.test(port) && Number(port) <= 65535) {
        const base = `http://127.0.0.1:${port}`;
        if ((await http(`${base}/api/health`)).status === 200) return base;
      }
    } catch { /* Ephemeral listener not ready. */ }
    await delay(30);
  }
  throw new Error("joint launcher startup deadline exceeded");
}
try {
  let base = await start();
  assert.equal((await http(`${base}/api/state`)).json().attachmentsAvailable, true);
  const uploaded = await http(`${base}/api/attachments`, "POST", gif);
  assert.equal(uploaded.status, 201);
  const id = uploaded.json().attachment.id;
  const report = (await core("")).json();
  assert.equal(report.status, "available"); assert.ok(report.usage.used_bytes > gif.length);
  assert.deepEqual(report.usage.reservation_ids, [], "successful upload commits its reservation");
  assert.equal((await http(`${base}/api/attachments/${id}`)).bytes.equals(gif), true);
  const sent = await http(`${base}/api/prompt`, "POST", JSON.stringify({ prompt: "remember this fixture image", attachment_ids: [id], mode: "queue" }), { "content-type": "application/json" });
  assert.equal(sent.status, 200);
  const deadline = Date.now() + 10000;
  let recorded = false;
  while (Date.now() < deadline) {
    const state = (await http(`${base}/api/state`)).json();
    recorded = state.thread?.turns.some(turn => turn.items.some(item => item.attachments?.some(image => image.id === id)));
    if (recorded) break; await delay(30);
  }
  assert.ok(recorded, "native fixture transcript binds the attachment");
  await stop(); base = await start();
  assert.equal((await http(`${base}/api/attachments/${id}`)).bytes.equals(gif), true, "launcher process restart retains the image");
  const reservation = await core("/reservations", "POST", { bytes: 100 });
  assert.equal(reservation.status, 201);
  const reservationId = reservation.json().reservation_id;
  assert.equal((await core(`/reservations/${reservationId}/commit`, "POST")).status, 200);
  assert.equal((await core(`/reservations/${reservationId}/commit`, "POST")).status, 409);
  const promises = [];
  try {
    for (let i = 0; i < 3; i++) {
      const reserved = await core("/reservations", "POST", { bytes: 64 * 1024 * 1024 });
      assert.equal(reserved.status, 201); promises.push(reserved.json().reservation_id);
    }
    const overCap = await core("/reservations", "POST", { bytes: 64 * 1024 * 1024 });
    assert.equal(overCap.status, 413);
    assert.equal(overCap.json().error.code, "private_state_agent_quota_exceeded");
  } finally {
    for (const promise of promises) assert.equal((await core(`/reservations/${promise}`, "DELETE")).status, 200);
  }
  assert.deepEqual((await core("")).json().usage.reservation_ids, []);
  await stop();
  console.log("Joint real Controller + launcher: upload, disk recount, commit, transcript binding, restart retention, repeated commit and quota rejection/release PASS");
  const purgeDeadline = Date.now() + (fixture.delete_after_seconds + 15) * 1000;
  while (existsSync(fixture.private_state_dir) && Date.now() < purgeDeadline) await delay(100);
  assert.equal(existsSync(fixture.private_state_dir), false, "confirmed deletion removes whole private directory");
  assert.notEqual((await core("/reservations", "POST", { bytes: 100 })).status, 201, "purged identity cannot recreate storage by quota request");
  assert.equal(existsSync(fixture.private_state_dir), false);
  console.log("Joint confirmed-deletion purge and no resurrection PASS");
} finally {
  await stop(); rmSync(cwd, { recursive: true, force: true });
}
