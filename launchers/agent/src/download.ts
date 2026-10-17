import type { IncomingMessage, ServerResponse } from "node:http";
import type { FileHandle } from "node:fs/promises";
import { contentDisposition, fileErrorStatus, openDownload } from "./files.js";

// GET /v1/files/download. One object owns the download slots so every exit path (open error, 413, empty file, client gone
// while the open is still pending, stream error, timeouts, normal end) releases exactly once and removes what it installed.
export const DOWNLOAD_LIMIT_BYTES = 1024 ** 3;
const MAX_CONCURRENT = 2;

function json(response: ServerResponse, status: number, body: unknown) {
  response.writeHead(status, { "cache-control": "no-store", "content-type": "application/json; charset=utf-8" });
  response.end(JSON.stringify(body));
}

export type DownloadOptions = {
  cwd: string;
  /** Open seam (tests delay it to disconnect a client mid-open). */
  open?: typeof openDownload;
  /** Idle time without socket activity before a stalled response is destroyed (AGENT_UI_DOWNLOAD_IDLE_MS). */
  idleMs?: number;
  /** One download lasts at most this long even if the client keeps trickling (AGENT_UI_DOWNLOAD_MAX_MS). */
  maxMs?: number;
  maxConcurrent?: number;
};

export function createDownloads(options: DownloadOptions) {
  const open = options.open ?? openDownload;
  const idleMs = Math.max(100, options.idleMs ?? 60_000);
  const maxMs = Math.max(100, options.maxMs ?? 60 * 60_000);
  const maxConcurrent = options.maxConcurrent ?? MAX_CONCURRENT;
  let active = 0;

  async function handle(request: IncomingMessage, response: ServerResponse, url: URL) {
    if (request.method !== "GET") {
      response.setHeader("allow", "GET");
      json(response, 405, { error: { code: "method_not_allowed", message: "File download is read-only; use GET" } });
      return;
    }
    // A page on another site must not be able to start a download in the user's browser (it could not read the file, but it
    // could fill the disk). Same-origin, same-site and user-typed (none) navigations through the Proxy are unaffected.
    if (request.headers["sec-fetch-site"] === "cross-site") {
      json(response, 403, { error: { code: "cross_site_download", message: "Downloads cannot be started from another site" } });
      return;
    }
    if (active >= maxConcurrent) {
      response.setHeader("retry-after", "2");
      json(response, 429, { error: { code: "too_many_downloads", message: "Too many downloads are running; try again in a moment" } });
      return;
    }
    // From here on the slot is held. The close handler is attached BEFORE the awaited open so a client that disconnects while
    // the open/stat is pending is cleaned up too.
    active++;
    let released = false;
    let handleClosed = false;
    let opened: { handle: FileHandle; size: number; name: string } | undefined;
    let stream: ReturnType<FileHandle["createReadStream"]> | undefined;
    let limitTimer: NodeJS.Timeout | undefined;
    const socket = response.socket;
    const onIdle = () => response.destroy();
    const cleanup = () => {
      if (!released) { released = true; active--; }
      if (limitTimer) clearTimeout(limitTimer);
      socket?.off("timeout", onIdle); // removes exactly our listener; Node's own keep-alive timer is left as it set it
      stream?.destroy();
      if (opened && !handleClosed) { handleClosed = true; void opened.handle.close().catch(() => {}); } // a no-op when the stream already auto-closed it
    };
    response.once("close", cleanup);
    try {
      try { opened = await open(options.cwd, url.searchParams.get("path") ?? ""); }
      catch (error) {
        cleanup();
        const status = fileErrorStatus(error);
        if (!response.destroyed) json(response, status, { error: { code: "file_access_failed", message: status === 404 ? "File not found" : status === 500 ? "Unable to read file" : error instanceof Error ? error.message : "File access denied" } });
        return;
      }
      if (response.destroyed || response.writableEnded) { cleanup(); return; } // the client left while the open was pending
      const { handle: file, size, name } = opened;
      // The size limit is decided from the open descriptor before any byte is sent.
      if (size > DOWNLOAD_LIMIT_BYTES) {
        cleanup();
        json(response, 413, { error: { code: "file_too_large", message: `This file is larger than the ${DOWNLOAD_LIMIT_BYTES / 1024 ** 3} GiB download limit`, limit_bytes: DOWNLOAD_LIMIT_BYTES, size } });
        return;
      }
      // Stream exactly the size seen on the open descriptor, so a file that grows meanwhile cannot break Content-Length.
      // No CSP sandbox header: Chromium treats a sandboxed response as a sandboxed download source and cancels it.
      if (size > 0) stream = file.createReadStream({ start: 0, end: size - 1, autoClose: true }); // before the headers: a failure here can still answer 500
      response.writeHead(200, {
        "content-type": "application/octet-stream", "content-length": size, "content-disposition": contentDisposition(name),
        "x-content-type-options": "nosniff", "cache-control": "no-store",
      });
      if (!stream) { response.end(); return; } // empty file; cleanup runs on the response close
      // A client that stops reading must not hold the descriptor: no socket activity for the idle time destroys the response.
      socket?.setTimeout(idleMs);
      socket?.on("timeout", onIdle);
      limitTimer = setTimeout(() => response.destroy(), maxMs);
      stream.once("error", () => response.destroy());
      stream.pipe(response);
    } catch (error) {
      // Anything unexpected between open and stream setup (close, writeHead, createReadStream): release and drop the response.
      cleanup();
      if (!response.headersSent && !response.destroyed) json(response, 500, { error: { code: "file_access_failed", message: "Unable to read file" } });
      else response.destroy();
    }
  }

  return { handle, active: () => active };
}
