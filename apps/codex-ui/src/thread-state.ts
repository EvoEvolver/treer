import { chmodSync, mkdirSync, openSync, closeSync, writeFileSync, fsyncSync, renameSync, readFileSync, unlinkSync } from "node:fs";
import { dirname, join } from "node:path";
import { randomUUID } from "node:crypto";

export function threadStatePath(cwd: string, agentId: string) {
  if (!agentId) return undefined;
  if (!/^[A-Za-z0-9_-]+$/.test(agentId)) throw new Error("Invalid TREER_AGENT_ID");
  return join(cwd, ".treer", "agents", agentId, "codex-ui.json");
}

export function loadThreadId(file?: string): string | undefined {
  if (!file) return undefined;
  let text: string;
  try { text = readFileSync(file, "utf8"); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
  const state = JSON.parse(text) as { threadId?: unknown };
  if (typeof state.threadId !== "string" || !state.threadId.trim()) {
    throw new Error(`Invalid saved Codex thread in ${file}; refusing to start a new thread`);
  }
  return state.threadId;
}

export function saveThreadId(file: string | undefined, threadId: string) {
  if (!file) return;
  const directory = dirname(file);
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  chmodSync(directory, 0o700);
  const temporary = `${file}.${randomUUID()}.tmp`;
  const fd = openSync(temporary, "wx", 0o600);
  try {
    writeFileSync(fd, JSON.stringify({ threadId }) + "\n");
    fsyncSync(fd);
  } finally { closeSync(fd); }
  try { renameSync(temporary, file); }
  finally { try { unlinkSync(temporary); } catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; } }
  const directoryFd = openSync(directory, "r");
  try { fsyncSync(directoryFd); } finally { closeSync(directoryFd); }
}
