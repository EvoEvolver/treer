import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, statSync, symlinkSync, unlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { Readable } from "node:stream";
import { deflateSync } from "node:zlib";
import type { TestContext } from "node:test";
import { AttachmentStore } from "../src/attachments.js";
import { AttachmentError, IMAGE_FILE_LIMIT, imageFormat } from "../src/image-format.js";

// A real 1x1 PNG, not merely a file containing a PNG signature.
function chunk(kind: string, data: Buffer) {
  const payload = Buffer.concat([Buffer.from(kind), data]);
  let crc = 0xffffffff;
  for (const byte of payload) { crc ^= byte; for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1)); }
  const size = Buffer.alloc(4), checksum = Buffer.alloc(4);
  size.writeUInt32BE(data.length); checksum.writeUInt32BE((crc ^ 0xffffffff) >>> 0);
  return Buffer.concat([size, payload, checksum]);
}
const PNG = Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]), chunk("IHDR", Buffer.from([0,0,0,1,0,0,0,1,8,2,0,0,0])), chunk("IDAT", deflateSync(Buffer.from([0,255,0,0]))), chunk("IEND", Buffer.alloc(0))]);
const GIF = Buffer.from("R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==", "base64");
function fixture(t: TestContext, quota?: number, now?: () => number) {
  const root = mkdtempSync(join(realpathSync.native(tmpdir()), "treer-image-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return { root, store: new AttachmentStore(join(root, "private"), quota, now) };
}
const code = (name: string) => (error: unknown) => error instanceof AttachmentError && error.code === name;

test("raster validation rejects MIME spoofing, truncation and appended payloads", () => {
  assert.equal(imageFormat(PNG).mime, "image/png");
  assert.equal(imageFormat(GIF).mime, "image/gif");
  for (const bytes of [Buffer.from("MZfake executable"), Buffer.from('<svg onload="alert(1)"/>'), PNG.subarray(0, -1), GIF.subarray(0, -1), Buffer.concat([PNG, Buffer.from("<script>x</script>")]), Buffer.concat([GIF, Buffer.from("MZ")])]) assert.throws(() => imageFormat(bytes), code("attachment_invalid_image"));
  const corrupt = Buffer.from(PNG); corrupt[45] ^= 1;
  assert.throws(() => imageFormat(corrupt), code("attachment_invalid_image"));
});
test("upload is private, opaque, session scoped, durable and resolved server-side", async t => {
  const { root, store } = fixture(t);
  const attachment = await store.upload(Readable.from([PNG.subarray(0, 20), PNG.subarray(20)]), "session", PNG.length);
  assert.match(attachment.id, /^img_[a-f0-9]{32}$/);
  assert.deepEqual(Object.keys(attachment).sort(), ["id", "mime", "size"]);
  assert.equal(store.read(attachment.id, "session").bytes.equals(PNG), true);
  assert.throws(() => store.read(attachment.id, "other-session"), code("attachment_not_found"));
  assert.throws(() => new AttachmentStore(join(root, "other-agent")).read(attachment.id, "session"), code("attachment_not_found"));
  assert.throws(() => store.read("../../secret", "session"), code("attachment_not_found"));
  const resolved = store.resolve([attachment.id], "session");
  assert.equal(resolved[0].data, PNG.toString("base64"));
  assert.equal(resolved[0].path, join(root, "private", `${attachment.id}.png`));
  store.bind("session", "user-item", [attachment.id]);
  const resumed = new AttachmentStore(join(root, "private"));
  assert.deepEqual(resumed.history("session", "user-item"), [attachment]);
  assert.throws(() => resumed.discard(attachment.id, "session"), code("attachment_already_sent"));
  if (process.platform !== "win32") {
    assert.equal(statSync(join(root, "private")).mode & 0o777, 0o700);
    for (const file of readdirSync(join(root, "private"))) assert.equal(statSync(join(root, "private", file)).mode & 0o777, 0o600);
  }
  assert.equal(readFileSync(join(root, "private", "attachments.json"), "utf8").includes(PNG.toString("base64")), false);
});
test("quota is reserved across concurrent streams; incomplete uploads leave no files", async t => {
  const { root, store } = fixture(t, PNG.length);
  let release!: () => void;
  const ready = new Promise<void>(resolve => { release = resolve; });
  const first = store.upload(Readable.from((async function* () { await ready; yield PNG; })()), "session", PNG.length);
  await assert.rejects(store.upload(Readable.from([PNG]), "session", PNG.length), code("attachment_quota_exceeded"));
  release(); await first;
  assert.equal(readdirSync(join(root, "private")).filter(file => file.endsWith(".tmp")).length, 0);
});
test("limits, empty, truncated and streaming oversize have fixed codes", async t => {
  const { root, store } = fixture(t);
  await assert.rejects(store.upload(Readable.from([]), "session", 0), code("attachment_empty"));
  await assert.rejects(store.upload(Readable.from([PNG]), "session", IMAGE_FILE_LIMIT + 1), code("attachment_too_large"));
  await assert.rejects(store.upload(Readable.from([PNG]), "session", PNG.length + 1), code("attachment_truncated"));
  await assert.rejects(store.upload(Readable.from([Buffer.alloc(IMAGE_FILE_LIMIT), Buffer.from([0])]), "session"), code("attachment_too_large"));
  assert.throws(() => store.resolve(Array(9).fill("x"), "session"), code("attachment_invalid_ids"));
  assert.equal(readdirSync(join(root, "private")).length, 0);
});
test("discard and expiry remove drafts but restart preserves retained images", async t => {
  let now = 0;
  const { root, store } = fixture(t, undefined, () => now);
  const sent = await store.upload(Readable.from([PNG]), "session", PNG.length);
  store.retain([sent.id], "session");
  const discarded = await store.upload(Readable.from([PNG]), "session", PNG.length);
  store.discard(discarded.id, "session");
  const expired = await store.upload(Readable.from([PNG]), "session", PNG.length);
  now += 3600001; store.sweep();
  assert.throws(() => store.read(expired.id, "session"), code("attachment_not_found"));
  assert.equal(new AttachmentStore(join(root, "private")).read(sent.id, "session").bytes.equals(PNG), true);
});
test("private roots and stored images cannot redirect through symlinks", { skip: process.platform === "win32" }, async t => {
  const { root, store } = fixture(t);
  symlinkSync(join(root, "private"), join(root, "link"), "dir");
  assert.throws(() => new AttachmentStore(join(root, "link", "images")), code("attachment_unsafe_storage"));
  const attachment = await store.upload(Readable.from([PNG]), "session", PNG.length);
  const path = join(root, "private", `${attachment.id}.png`);
  const outside = join(root, "outside.png"); writeFileSync(outside, PNG);
  unlinkSync(path); symlinkSync(outside, path);
  assert.throws(() => store.read(attachment.id, "session"), code("attachment_unsafe_storage"));
});

test("the prompt count and aggregate 24MiB limit are checked independently of file limits", async t => {
  const { store } = fixture(t);
  const large = Buffer.concat([PNG.subarray(0, -12), chunk("tEXt", Buffer.alloc(9 * 1024 * 1024)), PNG.subarray(-12)]);
  const ids: string[] = [];
  for (let index = 0; index < 3; index++) ids.push((await store.upload(Readable.from([large]), "session", large.length)).id);
  assert.throws(() => store.resolve(ids, "session"), code("attachment_prompt_too_large"));
  assert.throws(() => store.resolve(Array(9).fill(ids[0]), "session"), code("attachment_invalid_ids"));
  assert.throws(() => store.resolve([ids[0], ids[0]], "session"), code("attachment_invalid_ids"));
});
