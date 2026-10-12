import { constants } from "node:fs";
import { lstat, open, realpath } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";
import type { BackgroundTask } from "./model.js";

export const TASK_OUTPUT_LIMIT = 64 * 1024;
const inside = (root: string, path: string) => {
  const rel = relative(root, path);
  return rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel);
};

/** Only a task's registered file can be read, within adapter-owned trusted roots. */
export async function taskOutput(task: BackgroundTask, roots: readonly string[], tail?: string | null) {
  if (!task.outputFile) throw new Error("Task output is not available");
  const count = tail == null ? TASK_OUTPUT_LIMIT : Number(tail);
  if (!Number.isInteger(count) || count < 1) throw new Error("tail must be a positive byte count");
  const limit = Math.min(count, TASK_OUTPUT_LIMIT);
  const path = resolve(task.outputFile);
  const canonical = await realpath(path);
  let allowed = false;
  for (const root of roots) {
    const canonicalRoot = await realpath(root).catch(() => null);
    if (canonicalRoot && inside(resolve(root), path) && inside(canonicalRoot, canonical)) allowed = true;
  }
  if (!allowed || (await lstat(path)).isSymbolicLink()) throw new Error("Task output path is outside trusted roots or is a symlink");
  const file = await open(canonical, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const stat = await file.stat();
    if (!stat.isFile()) throw new Error("Task output must be a regular file");
    const buffer = Buffer.alloc(Math.min(limit, stat.size));
    const { bytesRead } = await file.read(buffer, 0, buffer.length, Math.max(0, stat.size - buffer.length));
    return { id: task.id, content: buffer.subarray(0, bytesRead).toString("utf8"), size: stat.size, truncated: stat.size > limit };
  } finally { await file.close(); }
}
