import { randomUUID } from "node:crypto";
import { createHash } from "node:crypto";
import { chmodSync, closeSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, realpathSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, parse, posix, relative, resolve, sep } from "node:path";
import { syncDirectory } from "./private-state.mjs";
import { buildWindowsShim } from "./windows-shim.mjs";

const marker = "treer-agent-shim/v1";
const present = path => { try { return lstatSync(path); } catch (error) { if (error.code === "ENOENT") return null; throw error; } };
export function shimText(node, entry, platform = process.platform) {
  if (platform === "win32") throw new Error("Windows requires a native PE launcher entry, not a shell shim");
  if (![node, entry].every(path => posix.isAbsolute(path) && !/[\x00-\x1f\x7f]/.test(path))) throw new Error("Shim requires absolute Node and launcher paths without control characters");
  const quote = text => "'" + text.replaceAll("'", "'\\''") + "'";
  return `#!/bin/sh\n# ${marker}\nexec ${quote(node)} ${quote(entry)} "$@"\n`;
}

export function shimPath(env = process.env, platform = process.platform) {
  const base = platform === "win32" ? env.LOCALAPPDATA : env.HOME || homedir();
  if (!isAbsolute(base || "")) throw new Error("A per-user home/local application directory is required for treer-agent");
  return platform === "win32" ? join(base, "Treer", "bin", "treer-agent.exe") : join(base, ".local", "bin", "treer-agent");
}

function directories(path) {
  let current = parse(resolve(path)).root;
  for (const part of relative(current, resolve(path)).split(sep).filter(Boolean)) {
    current = join(current, part);
    if (!present(current)) mkdirSync(current, { mode: 0o700 });
    const info = lstatSync(current);
    if (!info.isDirectory() || info.isSymbolicLink()) throw new Error("Shim directory ancestors must be physical directories, not links; use a physical home/bin directory (symlinked ~/.local is unsupported)");
  }
  const info = lstatSync(path);
  if (process.getuid && (info.uid !== process.getuid() || (info.mode & 0o022))) throw new Error("Shim bin directory must be owned by this user and not writable by other users");
}
function atomic(path, bytes, mode = 0o700) {
  const temporary = join(dirname(path), `.treer-agent-${randomUUID()}.tmp`);
  const fd = openSync(temporary, "wx", mode);
  try {
    try { writeFileSync(fd, bytes); fsyncSync(fd); } finally { closeSync(fd); }
    if (process.platform !== "win32") chmodSync(temporary, mode);
    renameSync(temporary, path); syncDirectory(dirname(path));
  } finally { if (present(temporary)) rmSync(temporary); }
}
/** Only our one named shim is touched. Return a rollback for installer failure. */
export function installShim(node, entry) {
  const path = shimPath();
  directories(dirname(path));
  const info = present(path);
  if (info && (!info.isFile() || info.isSymbolicLink() || process.getuid && info.uid !== process.getuid())) throw new Error("Existing treer-agent is not an owned regular shim");
  const previous = info ? readFileSync(path) : null;
  const digest = bytes => createHash("sha256").update(bytes).digest("hex");
  const receiptPath = path + ".json";
  const receiptInfo = present(receiptPath);
  if (receiptInfo && (!receiptInfo.isFile() || receiptInfo.isSymbolicLink())) throw new Error("Invalid native shim receipt");
  const oldReceipt = receiptInfo ? readFileSync(receiptPath) : null;
  const receipt = oldReceipt ? JSON.parse(oldReceipt.toString("utf8")) : null;
  if (previous && (process.platform === "win32" ? receipt?.schema !== marker || receipt.sha256 !== digest(previous) : !previous.toString("utf8").startsWith(`#!/bin/sh\n# ${marker}\n`))) throw new Error("Existing treer-agent is not a managed launcher shim; preserve it before installing");
  const pinnedNode = realpathSync.native(node);
  const same = previous && receipt?.node === pinnedNode && receipt.entry === entry && receipt.sha256 === digest(previous);
  const bytes = same ? previous : process.platform === "win32" ? buildWindowsShim(pinnedNode, entry.replace(/^\\\\\?\\([A-Za-z]:\\)/, "$1"), dirname(path)) : Buffer.from(shimText(pinnedNode, entry));
  const rollback = () => {
    if (!readFileSync(path).equals(bytes)) throw new Error("Shim changed during installation; refusing to overwrite it during rollback");
    if (previous) atomic(path, previous, info.mode & 0o777);
    else rmSync(path);
    if (oldReceipt) atomic(receiptPath, oldReceipt, 0o600);
    else rmSync(receiptPath);
  };
  try {
    if (!previous?.equals(bytes)) atomic(path, bytes);
    atomic(receiptPath, JSON.stringify({ schema: marker, node: pinnedNode, entry, sha256: digest(bytes) }) + "\n", 0o600);
  } catch (error) {
    if (present(path) && readFileSync(path).equals(bytes)) rollback();
    throw error;
  }
  return { path, rollback };
}
