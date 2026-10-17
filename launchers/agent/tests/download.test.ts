import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer, get, type IncomingMessage, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { createDownloads, DOWNLOAD_LIMIT_BYTES } from "../src/download.js";
import type { openDownload } from "../src/files.js";

type Opened = Awaited<ReturnType<typeof openDownload>>;
const deferred = <T>() => { let resolve!: (value: T) => void, reject!: (error: unknown) => void; const promise = new Promise<T>((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
const until = async (check: () => boolean, message: string) => { for (let i = 0; i < 100 && !check(); i++) await new Promise((r) => setTimeout(r, 20)); assert.ok(check(), message); };

async function serve(open: typeof openDownload) {
  const downloads = createDownloads({ cwd: "/unused", open });
  const server: Server = createServer((request, response) => { void downloads.handle(request, response, new URL(request.url!, "http://x")); });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const url = `http://127.0.0.1:${(server.address() as AddressInfo).port}/v1/files/download?path=f`;
  return { downloads, url, close: () => { server.closeAllConnections(); return new Promise<void>((resolve) => server.close(() => resolve())); } };
}
const fakeHandle = (overrides: Record<string, unknown> = {}) => {
  const calls = { close: 0 };
  const handle = { close: async () => { calls.close++; }, createReadStream: () => { throw new Error("must not stream"); }, ...overrides };
  return { calls, opened: { handle, size: 10, name: "f" } as unknown as Opened };
};

test("a client that disconnects while the open is pending frees the slot and the descriptor", async () => {
  const gate = deferred<Opened>();
  const server = await serve(() => gate.promise);
  try {
    const { calls, opened } = fakeHandle();
    const request = get(server.url); request.on("error", () => {});
    await until(() => server.downloads.active() === 1, "the slot is taken while the open is pending");
    request.destroy(); // the client leaves before the open finishes
    await until(() => server.downloads.active() === 0, "the slot is released as soon as the client is gone");
    gate.resolve(opened); // the filesystem open completes late
    await until(() => calls.close === 1, "the late descriptor is closed");
    await new Promise((r) => setTimeout(r, 50));
    assert.equal(calls.close, 1, "closed exactly once");
    assert.equal(server.downloads.active(), 0, "the slot is not released twice");
  } finally { await server.close(); }
});

test("an open that fails after the client left neither throws nor releases twice", async () => {
  const gate = deferred<Opened>();
  const server = await serve(() => gate.promise);
  try {
    const request = get(server.url); request.on("error", () => {});
    await until(() => server.downloads.active() === 1, "slot taken");
    request.destroy();
    await until(() => server.downloads.active() === 0, "slot released on disconnect");
    gate.reject(Object.assign(new Error("gone"), { code: "ENOENT" }));
    await new Promise((r) => setTimeout(r, 50));
    assert.equal(server.downloads.active(), 0);
  } finally { await server.close(); }
});

test("a failure after the open (createReadStream) answers 500, closes the descriptor and releases the slot", async () => {
  const { calls, opened } = fakeHandle({ createReadStream: () => { throw new Error("boom"); } });
  const server = await serve(async () => opened);
  try {
    const response = await new Promise<{ status: number; body: string }>((resolve, reject) => {
      get(server.url, (res: IncomingMessage) => { let body = ""; res.on("data", (c) => { body += c; }); res.on("end", () => resolve({ status: res.statusCode!, body })); }).on("error", reject);
    });
    assert.equal(response.status, 500);
    assert.equal(JSON.parse(response.body).error.code, "file_access_failed");
    await until(() => calls.close === 1, "descriptor closed");
    assert.equal(server.downloads.active(), 0);
  } finally { await server.close(); }
});

test("the size limit constant is 1 GiB", () => { assert.equal(DOWNLOAD_LIMIT_BYTES, 1073741824); });
