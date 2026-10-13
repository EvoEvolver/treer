import { existsSync, lstatSync, readFileSync, openSync, writeFileSync, fsyncSync, closeSync, renameSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { dirname } from "node:path";

export function readGoalState(file: string): unknown {
  if (!existsSync(file)) return undefined;
  const info = lstatSync(file);
  if (!info.isFile() || info.isSymbolicLink() || info.size > 8 * 1024 * 1024) throw new Error("Invalid goal state file");
  return JSON.parse(readFileSync(file, "utf8"));
}
export function writeGoalState(file: string, value: unknown) {
  if (existsSync(file) && (lstatSync(file).isSymbolicLink() || !lstatSync(file).isFile())) throw new Error("Invalid goal state file");
  const temp = `${file}.${randomUUID()}.tmp`, fd = openSync(temp, "wx", 0o600);
  try { writeFileSync(fd, JSON.stringify(value)); fsyncSync(fd); } finally { closeSync(fd); }
  renameSync(temp, file);
  const parent = openSync(dirname(file), "r"); try { fsyncSync(parent); } finally { closeSync(parent); }
}
