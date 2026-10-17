import { constants } from "node:fs";
import { open, readdir, realpath, stat } from "node:fs/promises";
import { basename, isAbsolute, relative, resolve, sep } from "node:path";

export const PREVIEW_LIMIT = 64 * 1024;

export class FileAccessError extends Error {
  constructor(message: string, readonly status = 400) { super(message); }
}

function inside(root: string, path: string) {
  const rel = relative(root, path);
  return rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel);
}

async function workspacePath(root: string, path: string) {
  if (path.includes("\0") || path.split(/[\\/]/).includes("..")) {
    throw new FileAccessError("Parent traversal is not allowed", 403);
  }
  const canonicalRoot = await realpath(root);
  const candidate = resolve(canonicalRoot, path || ".");
  if (!inside(canonicalRoot, candidate)) throw new FileAccessError("Path is outside the working directory", 403);
  const canonical = await realpath(candidate);
  if (!inside(canonicalRoot, canonical)) throw new FileAccessError("Symlink points outside the working directory", 403);
  return { canonical, path: relative(canonicalRoot, candidate).split(sep).join("/") };
}

export async function listFiles(root: string, path = "") {
  const directory = await workspacePath(root, path);
  if (!(await stat(directory.canonical)).isDirectory()) throw new FileAccessError("Not a directory");
  const entries = await readdir(directory.canonical, { withFileTypes: true });
  const nodes = [];
  for (const entry of entries) {
    const childPath = [directory.path, entry.name].filter(Boolean).join("/");
    try {
      const child = await workspacePath(root, childPath);
      const metadata = await stat(child.canonical);
      if (!metadata.isDirectory() && !metadata.isFile()) continue;
      nodes.push({ name: entry.name, path: childPath, kind: metadata.isDirectory() ? "directory" : "file",
        size: metadata.isFile() ? metadata.size : null, has_children: metadata.isDirectory() });
    } catch (error) {
      // Outside/broken symlinks are not navigable; other errors remain visible.
      if (error instanceof FileAccessError || ["ENOENT", "ELOOP"].includes((error as NodeJS.ErrnoException).code ?? "")) continue;
      throw error;
    }
  }
  nodes.sort((a, b) => a.kind.localeCompare(b.kind) || a.name.localeCompare(b.name));
  return { path: directory.path, entries: nodes };
}

export async function previewFile(root: string, path: string) {
  const file = await workspacePath(root, path);
  // Open only a canonical regular file; never follow a swapped final symlink.
  const handle = await open(file.canonical, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const metadata = await handle.stat();
    if (!metadata.isFile()) throw new FileAccessError("Not a regular file");
    const bytes = Buffer.alloc(PREVIEW_LIMIT + 1);
    let length = 0;
    while (length < bytes.length) {
      const read = await handle.read(bytes, length, bytes.length - length, null);
      if (!read.bytesRead) break;
      length += read.bytesRead;
    }
    return { path: file.path, name: basename(file.path), content: bytes.subarray(0, Math.min(length, PREVIEW_LIMIT)).toString("utf8"),
      size: metadata.size, truncated: length > PREVIEW_LIMIT };
  } finally { await handle.close(); }
}

/** An open canonical regular file for streaming; never reads it into memory and never follows a swapped final symlink. */
export async function openDownload(root: string, path: string) {
  if (!path) throw new FileAccessError("A file path is required");
  const file = await workspacePath(root, path);
  const handle = await open(file.canonical, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const metadata = await handle.stat();
    if (!metadata.isFile()) throw new FileAccessError("Not a regular file");
    return { handle, size: metadata.size, name: basename(file.path) };
  } catch (error) { await handle.close(); throw error; }
}

/** RFC 6266 attachment header: an ASCII fallback plus a UTF-8 filename* (RFC 5987 percent-encoding). */
export function contentDisposition(name: string) {
  const fallback = name.replace(/[^\x20-\x7e]/g, "_").replace(/["\\%;]/g, "_").replace(/[\x00-\x1f\x7f]/g, "_") || "download";
  const encoded = encodeURIComponent(name).replace(/['()*!]/g, (char) => `%${char.charCodeAt(0).toString(16).toUpperCase()}`);
  return `attachment; filename="${fallback}"; filename*=UTF-8''${encoded}`;
}

export function fileErrorStatus(error: unknown) {
  if (error instanceof FileAccessError) return error.status;
  const code = (error as NodeJS.ErrnoException).code;
  return code === "ENOENT" || code === "ENOTDIR" ? 404 : code === "EACCES" || code === "EPERM" || code === "ELOOP" ? 403 : 500;
}
