import assert from "node:assert/strict";
import test from "node:test";
import { createServer } from "node:http";
import { fetchRelease, releaseBase, releasePath, releasePlatform, validateFiles } from "../scripts/latest-download.mjs";

async function server(t, handler) {
  const http = createServer(handler);
  await new Promise(resolve => http.listen(0, "127.0.0.1", resolve));
  t.after(() => { http.closeAllConnections(); http.close(); });
  return releaseBase(`http://127.0.0.1:${http.address().port}/`);
}

test("platform and object selection refuses unsupported targets and URL/path injection", () => {
  assert.equal(releasePlatform("darwin", "arm64"), "darwin-aarch64");
  assert.equal(releasePlatform("win32", "x64"), "windows-x86_64");
  assert.throws(() => releasePlatform("linux", "ia32"));
  for (const path of ["../x", "/x", "a//b", "a%2fb", "a?b", "a\\b", "a:b"]) assert.throws(() => releasePath(path));
  for (const base of ["http://example.com/", "https://user@example.com/", "https://example.com/?x"]) assert.throws(() => releaseBase(base));
});

test("reject file/directory collisions and excess decoded file limits before staging", () => {
  const file = { path: "scripts/example.mjs", size: 1, sha256: "a".repeat(64), executable: false };
  const manifest = { schema: "treer.launcher-payload/v1", source_commit: "a".repeat(40), files: [file] };
  assert.throws(() => validateFiles({ ...manifest, files: [file, { ...file, path: "scripts" }] }), /Conflicting/);
  assert.throws(() => validateFiles({ ...manifest, files: [{ ...file, size: 33 * 1024 * 1024 }] }));
});

test("redirect, declared oversize, truncation and deadlines fail without following another object", async t => {
  let calls = 0;
  const base = await server(t, (request, response) => {
    calls++;
    if (request.url === "/redirect") { response.writeHead(302, { Location: "/target" }); response.end(); }
    else if (request.url === "/large") { response.writeHead(200, { "Content-Length": "9000" }); response.end("x"); }
    else if (request.url === "/cut") { response.writeHead(200, { "Content-Length": "10" }); response.flushHeaders(); response.write("x"); setImmediate(() => response.destroy()); }
    else if (request.url !== "/hang") response.end("x");
  });
  await assert.rejects(fetchRelease(base, "redirect", 10, AbortSignal.timeout(2000)));
  assert.equal(calls, 1);
  await assert.rejects(fetchRelease(base, "large", 10, AbortSignal.timeout(2000)), /limit/);
  await assert.rejects(fetchRelease(base, "cut", 10, AbortSignal.timeout(2000)));
  await assert.rejects(fetchRelease(base, "hang", 10, AbortSignal.timeout(25)));
});
