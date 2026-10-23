import assert from "node:assert/strict";
import test from "node:test";
import { previewBytes } from "../ui/src/api.js";
import { previewKind } from "../ui/src/components/previews/kind.js";

test("only explicit passive preview formats get rich viewers", () => {
  for (const [path, kind] of [["a.MD", "markdown"], ["a.markdown", "markdown"], ["a.PNG", "image"], ["a.pdf", "pdf"], ["a.svg", "text"], ["a.html", "text"], ["a.ts", "text"]]) assert.equal(previewKind(path!), kind);
});
test("preview download enforces streamed bytes even with stale/missing Content-Length and cancels", async t => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  let cancelled = false;
  globalThis.fetch = async () => new Response(new ReadableStream({ pull(c) { c.enqueue(new Uint8Array(21 * 1024 * 1024)); }, cancel() { cancelled = true; } }));
  await assert.rejects(previewBytes("big.pdf", new AbortController().signal), /preview_too_large/);
  assert.equal(cancelled, true);
});
test("preview download cancels before reading an oversized descriptor and retains relative API", async t => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  let cancelled = false;
  globalThis.fetch = async (path) => {
    assert.equal(path, "v1/files/download?path=space%20%23.png");
    return new Response(new ReadableStream({ cancel() { cancelled = true; } }), { headers: { "content-length": String(21 * 1024 * 1024) } });
  };
  await assert.rejects(previewBytes("space #.png", new AbortController().signal), /preview_too_large/);
  assert.equal(cancelled, true);
});
