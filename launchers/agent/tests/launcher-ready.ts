import type { ChildProcess } from "node:child_process";
import { readFile } from "node:fs/promises";
import { setTimeout as delay } from "node:timers/promises";
import { localFetch } from "./hermetic.mjs";

/** Wait for the actual listener AND initialized harness; never return a partial URL. */
export async function launcherReady(child: ChildProcess, portFile: string, timeoutMs = 30_000): Promise<string> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null || child.signalCode !== null) throw new Error("Launcher exited before becoming ready");
    try {
      const text = (await readFile(portFile, "utf8")).trim();
      const port = Number(text);
      if (/^\d+$/.test(text) && Number.isInteger(port) && port > 0 && port <= 65535) {
        const base = `http://127.0.0.1:${port}`;
        const remaining = deadline - Date.now();
        if (remaining <= 0) break;
        const response = await localFetch(`${base}/api/health`, { signal: AbortSignal.timeout(remaining) });
        await response.body?.cancel();
        if (response.ok) return base;
      }
    } catch { /* Port file may not exist yet; a listening harness may still be initializing. */ }
    await delay(Math.min(25, Math.max(0, deadline - Date.now())));
  }
  throw new Error("Launcher readiness timed out before a valid listening address and healthy harness were available");
}
