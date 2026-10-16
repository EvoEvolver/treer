import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Readable } from "node:stream";
import { AttachmentStore } from "../src/attachments.js";
import { PrivateStateClient } from "../src/private-state.js";
import { fakePrivateState } from "./fake-private-state.mjs";

const GIF = Buffer.from("R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==", "base64");
test("an upload aborted while awaiting quota releases its reservation without writing bytes", async () => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "quota-abort-"));
  const stream = new Readable({ read() {} }); let released = false;
  const store = new AttachmentStore(root, undefined, undefined, {
    async reserve() { stream.destroy(new Error("synthetic abort")); await new Promise(resolve => setImmediate(resolve)); return "fixture"; },
    async commit() { assert.fail("aborted upload must not commit"); },
    async release() { released = true; },
  });
  try {
    await assert.rejects(store.upload(stream, "session", GIF.length), { code: "attachment_truncated" });
    assert.equal(released, true); assert.deepEqual(readdirSync(root), []);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
test("Controller quota admission precedes bytes, durable file+journal precede commit, and failures roll back", async () => {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "quota-images-"));
  const controller = await fakePrivateState("ag_quota");
  try {
    const quota = new PrivateStateClient(controller.url, "ag_quota", "fixture-only");
    assert.equal(await quota.status(), "available");
    const store = new AttachmentStore(root, undefined, undefined, quota);
    controller.options.onCommit = () => {
      const journal = JSON.parse(readFileSync(join(root, "attachments.json"), "utf8"));
      assert.equal(journal.entries.length, 1);
      assert.ok(readdirSync(root).some(file => file.endsWith(".gif")));
    };
    const image = await store.upload(Readable.from([GIF]), "session", GIF.length);
    assert.equal(controller.reservations.size, 0);
    assert.deepEqual(controller.events.slice(1).map(event => event.method), ["POST", "POST"]);
    store.discard(image.id, "session");
    for (const code of ["private_state_global_quota_exceeded", "private_state_reservation_expired"]) {
      controller.options.commitError = code;
      await assert.rejects(store.upload(Readable.from([GIF]), "session", GIF.length), { code });
      assert.deepEqual(JSON.parse(readFileSync(join(root, "attachments.json"), "utf8")).entries, []);
      assert.deepEqual(readdirSync(root), ["attachments.json"]);
      assert.equal(controller.reservations.size, 0);
      assert.equal(controller.events.at(-1)?.method, "DELETE");
    }
    controller.options.commitError = undefined;
    controller.options.reserveError = "private_state_global_quota_exceeded";
    let read = false;
    const bytes = Readable.from((function* () { read = true; yield GIF; })());
    await assert.rejects(store.upload(bytes, "session", GIF.length), { code: "private_state_global_quota_exceeded" });
    assert.equal(read, false, "no upload bytes consumed before Core admission");
    assert.deepEqual(readdirSync(root), ["attachments.json"]);
    controller.options.reserveError = undefined;
    controller.options.hangCommit = true;
    const bounded = new PrivateStateClient(controller.url, "ag_quota", "fixture-only", 1000);
    const timedOut = new AttachmentStore(root, undefined, undefined, bounded);
    await assert.rejects(timedOut.upload(Readable.from([GIF]), "session", GIF.length), { code: "agent_private_state_unavailable" });
    assert.deepEqual(JSON.parse(readFileSync(join(root, "attachments.json"), "utf8")).entries, []);
    assert.deepEqual(readdirSync(root), ["attachments.json"]);
    assert.equal(controller.reservations.size, 0, "unknown commit outcome rolls back and releases");
  } finally { await controller.close(); rmSync(root, { recursive: true, force: true }); }
});

test("quota status is path-free, credentials stay bound to local Controller and repeated/foreign commits fail", async () => {
  const controller = await fakePrivateState("ag_owner");
  try {
    const quota = new PrivateStateClient(controller.url, "ag_owner", "fixture-only");
    const id = await quota.reserve(100);
    await quota.commit(id);
    await assert.rejects(quota.commit(id), { code: "private_state_reservation_expired" });
    await quota.release(id); await quota.release(id);
    await assert.rejects(new PrivateStateClient(controller.url, "ag_other", "fixture-only").status(), { code: "agent_private_state_unavailable" });
    for (const status of ["agent_private_state_unsafe", "agent_private_state_inside_cwd", "agent_private_state_unavailable"]) {
      controller.options.status = status; assert.equal(await quota.status(), status);
    }
    for (const url of ["https://127.0.0.1", "http://example.com", "http://secret@127.0.0.1", `${controller.url}/evil`]) {
      assert.throws(() => new PrivateStateClient(url, "ag_owner", "fixture-only"));
    }
  } finally { await controller.close(); }
});
