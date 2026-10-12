import { createHash, randomUUID } from "node:crypto";
import { constants } from "node:fs";
import { lstat, mkdir, mkdtemp, open, readdir, realpath, rename, rm, rmdir, symlink } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { SessionTransferError, type ImportedSession, type SessionBundle, type SessionFile, type SessionImportOptions } from "../session-transfer.js";

type NativeHarness = "codex" | "claude";
const UUID = /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/i;
const MAX_BYTES = 64 * 1024 * 1024;
const MAX_FILES = 1024;
const instructions = new Set(["AGENTS.md", "CLAUDE.md", "MEMORY.md", ".claude/CLAUDE.md"]);
const digest = (bytes: Buffer) => createHash("sha256").update(bytes).digest("hex");
const projectKey = (cwd: string) => cwd.replace(/[^a-zA-Z0-9]/g, "-");
const nativeHome = (harness: NativeHarness) => harness === "codex" ? process.env.CODEX_HOME || join(homedir(), ".codex") : process.env.CLAUDE_CONFIG_DIR || join(homedir(), ".claude");
function invalid(message: string): never { throw new SessionTransferError("invalid_bundle", message); }

function safePath(path: string) {
  if (!path || path.includes("\\") || path.includes("\0") || isAbsolute(path) || /^[A-Za-z]:/.test(path) || path.split("/").some((part) => !part || part === "." || part === "..")) invalid("Unsafe session file path");
  return path;
}
async function exists(path: string) {
  try { await lstat(path); return true; }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return false; throw error; }
}
async function regularFile(path: string, root: string) {
  const inside = relative(root, path);
  if (inside === ".." || inside.startsWith(`..${sep}`) || isAbsolute(inside)) invalid("Session file is outside its storage root");
  let current = root;
  for (const part of inside.split(sep)) {
    current = join(current, part);
    if ((await lstat(current)).isSymbolicLink()) invalid("Session transfer rejects symbolic links");
  }
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const before = await handle.stat();
    if (!before.isFile()) invalid("Session transfer accepts only regular files");
    if (before.size > MAX_BYTES) throw new SessionTransferError("too_large", "Session snapshot exceeds 64 MiB");
    const bytes = Buffer.alloc(before.size);
    let offset = 0;
    while (offset < bytes.length) {
      const { bytesRead } = await handle.read(bytes, offset, bytes.length - offset, offset);
      if (!bytesRead) break;
      offset += bytesRead;
    }
    const after = await handle.stat();
    if (offset !== bytes.length || before.size !== after.size || before.mtimeMs !== after.mtimeMs) throw new SessionTransferError("not_ready", "Session changed during export; retry when idle");
    return bytes;
  } finally { await handle.close(); }
}
async function walk(root: string, match: (path: string) => boolean, prefix = "", depth = 0): Promise<string[]> {
  if (!(await exists(join(root, prefix)))) return [];
  if (depth > 10) throw new SessionTransferError("too_large", "Session directory is too deep");
  const files: string[] = [];
  for (const entry of await readdir(join(root, prefix), { withFileTypes: true })) {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isSymbolicLink()) invalid("Session transfer rejects symbolic links");
    if (entry.isDirectory()) files.push(...await walk(root, match, path, depth + 1));
    else if (entry.isFile() && match(path)) files.push(path);
    if (files.length > MAX_FILES) throw new SessionTransferError("too_large", "Too many session files");
  }
  return files.sort();
}
function records(bytes: Buffer): Record<string, unknown>[] {
  if (!bytes.length || bytes.at(-1) !== 10) invalid("Native session must contain complete JSONL records");
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes).split("\n").filter(Boolean).map((line) => {
      const value: unknown = JSON.parse(line);
      if (!value || typeof value !== "object" || Array.isArray(value)) invalid("Session record must be an object");
      return value as Record<string, unknown>;
    });
  } catch (error) { if (error instanceof SessionTransferError) throw error; return invalid("Invalid native session JSONL"); }
}
function checkSession(harness: NativeHarness, sessionId: string, bytes: Buffer) {
  const rows = records(bytes);
  if (harness === "codex") {
    const first = rows[0];
    if (first?.type !== "session_meta" || (first.payload as Record<string, unknown> | undefined)?.id !== sessionId) invalid("Codex session metadata does not match session ID");
  } else if (!rows.some((row) => row.sessionId === sessionId)) invalid("Claude session metadata does not match session ID");
}

/** Explicit allowlist: never read auth, settings, shell snapshots or arbitrary project files. */
export async function exportNativeSession(harness: NativeHarness, sessionId: string, cwd: string, providerHome = nativeHome(harness)): Promise<SessionBundle> {
  if (!UUID.test(sessionId)) invalid("Native session ID must be a UUID");
  const project = await realpath(cwd), home = await realpath(providerHome);
  let source: string;
  if (harness === "codex") {
    const root = join(home, "sessions");
    const matches = await walk(root, (path) => path.endsWith(`-${sessionId}.jsonl`));
    if (matches.length !== 1) throw new SessionTransferError("not_ready", "Cannot identify exactly one native Codex session");
    source = join(root, matches[0]);
  } else {
    const preferred = join(home, "projects", projectKey(project), `${sessionId}.jsonl`);
    if (await exists(preferred)) source = preferred;
    else {
      const root = join(home, "projects");
      const matches = (await exists(root)) ? (await readdir(root, { withFileTypes: true })).filter((entry) => entry.isDirectory()).map((entry) => join(root, entry.name, `${sessionId}.jsonl`)) : [];
      const found = (await Promise.all(matches.map(async (path) => await exists(path) ? path : null))).filter((path): path is string => path !== null);
      if (found.length !== 1) throw new SessionTransferError("not_ready", "Cannot identify exactly one native Claude session");
      source = found[0];
    }
  }
  const files: SessionFile[] = [];
  let total = 0;
  const add = async (scope: SessionFile["scope"], path: string, source: string, root: string) => {
    const bytes = await regularFile(source, root);
    total += bytes.length;
    if (total > MAX_BYTES || files.length >= MAX_FILES) throw new SessionTransferError("too_large", "Session snapshot exceeds size/file limits");
    files.push({ scope, path, encoding: "base64", content: bytes.toString("base64"), sha256: digest(bytes) });
    return bytes;
  };
  const main = await add("session", "session.jsonl", source, home);
  checkSession(harness, sessionId, main);
  if (harness === "claude") {
    const subagents = join(dirname(source), sessionId, "subagents");
    if (await exists(subagents)) {
      for (const path of await walk(subagents, (path) => path.endsWith(".jsonl"))) await add("session", `subagents/${path}`, join(subagents, path), home);
    }
    const memory = join(dirname(source), "memory");
    if (await exists(memory)) for (const path of await walk(memory, (path) => path.endsWith(".md"))) await add("memory", path, join(memory, path), home);
  }
  for (const path of instructions) if (await exists(join(project, path))) await add("project", path, join(project, path), project);
  for (const prefix of [".codex/memory", ".claude/memory"]) {
    const root = join(project, prefix);
    if (await exists(root)) for (const path of await walk(root, (path) => path.endsWith(".md"))) await add("project", `${prefix}/${path}`, join(root, path), project);
  }
  return { schema: "treer.agent-session/v1", harness, sessionId, cwd: project, exportedAt: new Date().toISOString(), files };
}

function decodeBundle(bundle: SessionBundle) {
  if (!bundle || bundle.schema !== "treer.agent-session/v1" || !["codex", "claude"].includes(bundle.harness) || typeof bundle.sessionId !== "string" || !UUID.test(bundle.sessionId) || typeof bundle.cwd !== "string" || !Array.isArray(bundle.files)) invalid("Unsupported or invalid session bundle");
  if (!bundle.files.length || bundle.files.length > MAX_FILES) throw new SessionTransferError("too_large", "Invalid session file count");
  const found = new Set<string>();
  let total = 0;
  const decoded = bundle.files.map((file) => {
    if (!file || typeof file.path !== "string" || typeof file.content !== "string" || file.encoding !== "base64" || !/^[a-f0-9]{64}$/.test(file.sha256)) invalid("Invalid session file entry");
    safePath(file.path);
    if (file.content.length > Math.ceil(MAX_BYTES / 3) * 4) throw new SessionTransferError("too_large", "Session file exceeds size limit");
    const bytes = Buffer.from(file.content, "base64");
    if (bytes.toString("base64") !== file.content || digest(bytes) !== file.sha256) invalid("Session file hash/encoding mismatch");
    total += bytes.length;
    if (total > MAX_BYTES) throw new SessionTransferError("too_large", "Session snapshot exceeds 64 MiB");
    const key = `${file.scope}/${file.path}`;
    if (found.has(key)) invalid("Duplicate session file path");
    found.add(key);
    const allowed = file.scope === "session" ? file.path === "session.jsonl" || (bundle.harness === "claude" && /^subagents\/.+\.jsonl$/.test(file.path))
      : file.scope === "memory" ? bundle.harness === "claude" && file.path.endsWith(".md")
      : file.scope === "project" && (instructions.has(file.path) || /^\.(?:codex|claude)\/memory\/.+\.md$/.test(file.path));
    if (!allowed) invalid("File is outside the session/memory/instruction allowlist");
    if (file.scope === "session") records(bytes);
    return { ...file, bytes };
  });
  const main = decoded.find((file) => file.scope === "session" && file.path === "session.jsonl");
  if (!main) invalid("Missing main session file");
  checkSession(bundle.harness as NativeHarness, bundle.sessionId, main.bytes);
  return decoded;
}
function rewriteSession(bytes: Buffer, harness: NativeHarness, id: string, cwd: string) {
  return Buffer.from(records(bytes).map((row) => {
    if ("cwd" in row) row.cwd = cwd;
    if ("sessionId" in row) row.sessionId = id;
    if ("session_id" in row) row.session_id = id;
    const payload = row.payload as Record<string, unknown> | undefined;
    if (payload && "cwd" in payload) payload.cwd = cwd;
    if (harness === "codex" && row.type === "session_meta" && payload) {
      payload.id = id; payload.cwd = cwd; delete payload.forked_from_id;
    }
    return JSON.stringify(row);
  }).join("\n") + "\n");
}
async function writePrivate(path: string, bytes: Buffer) {
  await mkdir(dirname(path), { recursive: true, mode: 0o700 });
  const handle = await open(path, "wx", 0o600);
  try { await handle.writeFile(bytes); await handle.sync(); } finally { await handle.close(); }
}

/** Restore a new native identity. Existing provider credentials stay local. */
export async function importNativeSession(bundle: SessionBundle, options: SessionImportOptions, providerHome = nativeHome(bundle.harness as NativeHarness)): Promise<ImportedSession> {
  const decoded = decodeBundle(bundle), harness = bundle.harness as NativeHarness;
  const id = options.sessionId ?? randomUUID();
  if (!UUID.test(id) || id === bundle.sessionId) invalid("Import requires a fresh native session UUID");
  if (typeof options.directory !== "string" || !options.directory.trim()) invalid("Import directory is required");
  if (harness === "claude" && await exists(join(providerHome, "projects"))) {
    for (const entry of await readdir(join(providerHome, "projects"), { withFileTypes: true })) {
      if ((entry.isDirectory() || entry.isSymbolicLink()) && await exists(join(providerHome, "projects", entry.name, `${id}.jsonl`))) throw new SessionTransferError("destination_exists", "Native Claude session UUID already exists");
    }
  }
  const requested = resolve(options.directory);
  await mkdir(dirname(requested), { recursive: true, mode: 0o700 });
  const destination = join(await realpath(dirname(requested)), basename(requested));
  let emptyExisting = false;
  if (await exists(destination)) {
    const info = await lstat(destination);
    if (!info.isDirectory() || info.isSymbolicLink() || (await readdir(destination)).length) throw new SessionTransferError("destination_exists", "Import destination must be new or empty");
    emptyExisting = true;
  }
  const stage = await mkdtemp(join(dirname(destination), ".session-import-"));
  const environment: Record<string, string> = {};
  let nativeProject: string | undefined;
  try {
    let sessionRoot: string, memoryRoot: string;
    if (harness === "codex") {
      const home = join(stage, ".harness/codex");
      sessionRoot = join(home, "sessions"); memoryRoot = join(home, "memory");
      environment.CODEX_HOME = join(destination, ".harness/codex");
      await mkdir(home, { recursive: true, mode: 0o700 });
      // Same-provider local references only; never read, copy or export auth.
      for (const name of ["auth.json", "config.toml"]) if (await exists(join(providerHome, name))) await symlink(await realpath(join(providerHome, name)), join(home, name));
    } else {
      const home = resolve(providerHome);
      await mkdir(join(home, "projects"), { recursive: true, mode: 0o700 });
      nativeProject = join(await realpath(join(home, "projects")), projectKey(destination));
      try { await mkdir(nativeProject, { mode: 0o700 }); }
      catch (error) { nativeProject = undefined; if ((error as NodeJS.ErrnoException).code === "EEXIST") throw new SessionTransferError("destination_exists", "Claude already has state for this destination"); throw error; }
      sessionRoot = nativeProject; memoryRoot = join(nativeProject, "memory");
      environment.CLAUDE_CONFIG_DIR = await realpath(home);
    }
    for (const file of decoded) {
      if (file.scope === "project") await writePrivate(join(stage, file.path), file.bytes);
      else if (file.scope === "memory") await writePrivate(join(memoryRoot, file.path), file.bytes);
      else {
        const path = file.path === "session.jsonl"
          ? harness === "claude" ? `${id}.jsonl` : `rollout-${new Date().toISOString().slice(0, 19).replaceAll(":", "-")}-${id}.jsonl`
          : `${id}/${file.path}`;
        await writePrivate(join(sessionRoot, path), rewriteSession(file.bytes, harness, id, destination));
      }
    }
    if (emptyExisting) await rmdir(destination);
    await rename(stage, destination);
    return { sessionId: id, cwd: destination, environment };
  } catch (error) {
    if (nativeProject) await rm(nativeProject, { recursive: true, force: true });
    throw error;
  } finally { await rm(stage, { recursive: true, force: true }); }
}
