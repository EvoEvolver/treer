import { createHash, randomBytes } from "node:crypto";
import { constants, existsSync, lstatSync, mkdirSync, openSync, closeSync, readFileSync, readdirSync, writeFileSync, fsyncSync, renameSync, unlinkSync, realpathSync, fstatSync } from "node:fs";
import { open } from "node:fs/promises";
import { isAbsolute, join, parse, relative, resolve, sep } from "node:path";
import type { Readable } from "node:stream";
import { protectDirectory, syncDirectory } from "../scripts/private-state.mjs";
import { ATTACHMENT_ID, AttachmentError, IMAGE_COUNT_LIMIT, IMAGE_FILE_LIMIT, IMAGE_PROMPT_LIMIT, imageFormat } from "./image-format.js";
import type { ImageAttachment } from "./model.js";
import type { HarnessImage } from "./runtime.js";

interface Entry extends ImageAttachment { session: string; extension: string; digest: string; createdAt: number; retained: boolean }
interface Journal { version: 1; entries: Entry[]; bindings: Record<string, string[]> }
const DRAFT_TTL = 60 * 60 * 1000;
const AGENT_QUOTA = 256 * 1024 * 1024;

/** Root is supplied by the Controller, outside cwd. Never derive it from a request.
 * The Controller owns deletion and global quota; this store only owns exact files
 * registered in this Agent's journal. Restart preserves sent attachments.
 */
export class AttachmentStore {
  private entries = new Map<string, Entry>();
  private bindings: Record<string, string[]> = Object.create(null);
  private reserved = 0;
  private activeUploads = new Set<string>();
  private readonly root: string;
  private readonly journal: string;
  constructor(root: string, readonly quota = AGENT_QUOTA, private readonly now = Date.now) {
    if (!isAbsolute(root)) throw new AttachmentError("attachment_private_root_required", 503);
    const resolved = resolve(root);
    let ancestor = parse(resolved).root;
    for (const part of relative(ancestor, resolved).split(sep).filter(Boolean)) {
      ancestor = join(ancestor, part);
      if (!existsSync(ancestor)) mkdirSync(ancestor, { mode: 0o700 });
      const info = lstatSync(ancestor);
      if (!info.isDirectory() || info.isSymbolicLink()) throw new AttachmentError("attachment_unsafe_storage", 503);
    }
    protectDirectory(resolved);
    this.root = realpathSync.native(resolved);
    this.journal = join(this.root, "attachments.json");
    if (existsSync(this.journal)) {
      const handle = this.openRegistered(this.journal, 16 * 1024 * 1024);
      let data: Journal;
      try { data = JSON.parse(readFileSync(handle, "utf8")); } finally { closeSync(handle); }
      if (data.version !== 1 || !Array.isArray(data.entries) || data.entries.length > 8192 || !data.bindings || typeof data.bindings !== "object" || Array.isArray(data.bindings)) throw new AttachmentError("attachment_invalid_journal", 503);
      for (const entry of data.entries) {
        if (!entry || !ATTACHMENT_ID.test(entry.id) || this.entries.has(entry.id) || !["png","jpg","webp","gif"].includes(entry.extension) || !["image/png","image/jpeg","image/webp","image/gif"].includes(entry.mime) || !Number.isInteger(entry.size) || entry.size < 1 || entry.size > IMAGE_FILE_LIMIT || typeof entry.session !== "string" || !entry.session || entry.session.length > 256 || !Number.isFinite(entry.createdAt) || typeof entry.retained !== "boolean") throw new AttachmentError("attachment_invalid_journal", 503);
        this.entries.set(entry.id, entry);
        if (!/^[a-f0-9]{64}$/.test(entry.digest)) throw new AttachmentError("attachment_invalid_journal", 503);
      }
      if (Object.keys(data.bindings).length > 16384) throw new AttachmentError("attachment_invalid_journal", 503);
      for (const [key, ids] of Object.entries(data.bindings)) {
        if (key.length > 1024 || !Array.isArray(ids) || ids.length > IMAGE_COUNT_LIMIT || ids.some(id => typeof id !== "string" || !this.entries.has(id))) throw new AttachmentError("attachment_invalid_journal", 503);
        this.bindings[key] = ids;
      }
    }
    this.sweep();
  }
  private openRegistered(path: string, limit: number) {
    // Re-check root and file on every access. Native realpath also accepts the
    // Controller's Windows verbatim paths; never strip boundary checks.
    try {
      const rootInfo = lstatSync(this.root), info = lstatSync(path);
      const inside = relative(realpathSync.native(this.root), realpathSync.native(path));
      if (!rootInfo.isDirectory() || rootInfo.isSymbolicLink() || !info.isFile() || info.isSymbolicLink() || info.size > limit || isAbsolute(inside) || inside === ".." || inside.startsWith(`..${sep}`)) throw new AttachmentError("attachment_unsafe_storage", 503);
      const fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
      const opened = fstatSync(fd);
      if (!opened.isFile() || opened.size > limit || opened.dev !== info.dev || opened.ino !== info.ino) { closeSync(fd); throw new AttachmentError("attachment_unsafe_storage", 503); }
      return fd;
    } catch { throw new AttachmentError("attachment_unsafe_storage", 503); }
  }
  private path(entry: Entry) { return join(this.root, `${entry.id}.${entry.extension}`); }
  private metadata(entry: Entry): ImageAttachment { return { id: entry.id, mime: entry.mime, size: entry.size }; }
  private save() {
    try { this.writeJournal(); }
    catch { throw new AttachmentError("attachment_storage_failed", 503); }
  }
  private writeJournal() {
    const temporary = join(this.root, `.journal-${randomBytes(16).toString("hex")}.tmp`);
    const handle = openSync(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
    try {
      writeFileSync(handle, JSON.stringify({ version: 1, entries: [...this.entries.values()], bindings: this.bindings }));
      fsyncSync(handle);
    } finally { closeSync(handle); }
    try { renameSync(temporary, this.journal); syncDirectory(this.root); }
    finally { if (existsSync(temporary)) unlinkSync(temporary); }
  }
  private entry(id: string, session: string) {
    const entry = ATTACHMENT_ID.test(id) ? this.entries.get(id) : undefined;
    if (!entry || entry.session !== session) throw new AttachmentError("attachment_not_found", 404);
    return entry;
  }
  /** Reserve before awaiting I/O, so concurrent uploads cannot race the quota. */
  async upload(stream: Readable, session: string, length?: number): Promise<ImageAttachment> {
    if (!session || session.length > 256) throw new AttachmentError("attachment_session_required", 409);
    if (length !== undefined && (!Number.isSafeInteger(length) || length < 0)) throw new AttachmentError("attachment_invalid_length");
    if (length === 0) throw new AttachmentError("attachment_empty");
    if (length !== undefined && length > IMAGE_FILE_LIMIT) throw new AttachmentError("attachment_too_large", 413);
    this.sweep();
    const reservation = length ?? IMAGE_FILE_LIMIT;
    const used = readdirSync(this.root).reduce((total, name) => {
      if (this.activeUploads.has(name)) return total;
      if (!/^(?:img_[a-f0-9]{32}\.(?:png|jpg|webp|gif)|\.img_[a-f0-9]{32}\.tmp)$/.test(name)) return total;
      const info = lstatSync(join(this.root, name));
      if (!info.isFile() || info.isSymbolicLink()) throw new AttachmentError("attachment_unsafe_storage", 503);
      return total + info.size;
    }, 0);
    if (used + this.reserved + reservation > this.quota || this.entries.size + this.activeUploads.size >= 8192) throw new AttachmentError("attachment_quota_exceeded", 413);
    this.reserved += reservation;
    const id = `img_${randomBytes(16).toString("hex")}`, temporary = join(this.root, `.${id}.tmp`);
    this.activeUploads.add(`.${id}.tmp`);
    let destination: string | undefined, fd: Awaited<ReturnType<typeof open>> | undefined;
    try {
      fd = await open(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
      let size = 0;
      for await (const chunk of stream.iterator({ destroyOnReturn: false })) {
        const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
        size += bytes.length;
        if (size > IMAGE_FILE_LIMIT || size > reservation) throw new AttachmentError("attachment_too_large", 413);
        await fd.writeFile(bytes);
      }
      if (length !== undefined && size !== length) throw new AttachmentError("attachment_truncated");
      await fd.sync(); await fd.close(); fd = undefined;
      const reader = this.openRegistered(temporary, IMAGE_FILE_LIMIT);
      let format: ReturnType<typeof imageFormat>, digest: string;
      try { const bytes = readFileSync(reader); format = imageFormat(bytes); digest = createHash("sha256").update(bytes).digest("hex"); } finally { closeSync(reader); }
      const entry: Entry = { id, ...format, digest, size, session, createdAt: this.now(), retained: false };
      destination = this.path(entry);
      renameSync(temporary, destination); syncDirectory(this.root);
      this.entries.set(id, entry);
      try { this.save(); } catch (error) { this.entries.delete(id); throw error; }
      destination = undefined;
      return this.metadata(entry);
    } catch (error) {
      if (error instanceof AttachmentError) throw error;
      if (stream.readableAborted) throw new AttachmentError("attachment_truncated");
      // I/O errors can contain local paths; expose only a fixed code.
      throw new AttachmentError("attachment_upload_failed", 503);
    } finally {
      this.reserved -= reservation;
      this.activeUploads.delete(`.${id}.tmp`);
      await fd?.close().catch(() => {});
      if (existsSync(temporary)) unlinkSync(temporary);
      if (destination && existsSync(destination)) unlinkSync(destination);
    }
  }
  read(id: string, session: string): { metadata: ImageAttachment; bytes: Buffer } {
    const entry = this.entry(id, session), fd = this.openRegistered(this.path(entry), IMAGE_FILE_LIMIT);
    let bytes: Buffer;
    try { bytes = readFileSync(fd); } finally { closeSync(fd); }
    const format = imageFormat(bytes);
    if (bytes.length !== entry.size || format.mime !== entry.mime || format.extension !== entry.extension) throw new AttachmentError("attachment_invalid_image");
    return { metadata: this.metadata(entry), bytes };
  }
  resolve(ids: unknown, session: string): HarnessImage[] {
    if (!Array.isArray(ids) || ids.length > IMAGE_COUNT_LIMIT || ids.some(id => typeof id !== "string") || new Set(ids).size !== ids.length) throw new AttachmentError("attachment_invalid_ids");
    const entries = ids.map(id => this.entry(id, session));
    if (entries.reduce((total, entry) => total + entry.size, 0) > IMAGE_PROMPT_LIMIT) throw new AttachmentError("attachment_prompt_too_large", 413);
    return entries.map(entry => {
      const { metadata, bytes } = this.read(entry.id, session);
      // root was physically resolved once and file access rejects links. Do not
      // re-resolve after closing the checked handle and accidentally return an
      // outside target if a local process swaps the path in that interval.
      return { ...metadata, path: this.path(entry), data: bytes.toString("base64") };
    });
  }
  retain(ids: string[], session: string) {
    for (const id of ids) this.entry(id, session).retained = true;
    this.save();
  }
  bind(session: string, itemId: string, ids: string[]) {
    this.resolve(ids, session);
    const key = JSON.stringify([session, itemId]);
    if (key.length > 1024 || !Object.hasOwn(this.bindings, key) && Object.keys(this.bindings).length >= 16384) throw new AttachmentError("attachment_quota_exceeded", 413);
    this.bindings[key] = ids;
    this.retain(ids, session);
  }
  history(session: string, itemId: string): ImageAttachment[] {
    return (this.bindings[JSON.stringify([session, itemId])] || []).map(id => this.metadata(this.entry(id, session)));
  }
  metadataFor(ids: string[], session: string): ImageAttachment[] {
    return ids.flatMap(id => { const entry = this.entries.get(id); return entry?.session === session ? [this.metadata(entry)] : []; });
  }
  metadataForDigests(digests: string[], session: string): ImageAttachment[] {
    return digests.flatMap(digest => {
      const entry = [...this.entries.values()].find(entry => entry.session === session && entry.retained && entry.digest === digest);
      return entry ? [this.metadata(entry)] : [];
    });
  }
  discard(id: string, session: string) {
    const entry = this.entry(id, session);
    if (entry.retained) throw new AttachmentError("attachment_already_sent", 409);
    this.remove(entry); this.save();
  }
  private remove(entry: Entry) {
    const path = this.path(entry);
    // Never follow links, even for deletion; reject tampered state.
    if (existsSync(path)) { const fd = this.openRegistered(path, IMAGE_FILE_LIMIT); closeSync(fd); unlinkSync(path); }
    this.entries.delete(entry.id);
  }
  sweep() {
    let changed = false;
    for (const entry of this.entries.values()) if (!entry.retained && this.now() - entry.createdAt >= DRAFT_TTL) { this.remove(entry); changed = true; }
    // Exact launcher-owned names only. A crash before journal commit can leave
    // a temporary upload or renamed draft; never enumerate any other directory.
    for (const name of readdirSync(this.root)) {
      if (this.activeUploads.has(name)) continue;
      const match = /^(?:\.(img_[a-f0-9]{32})\.tmp|(img_[a-f0-9]{32})\.(?:png|jpg|webp|gif)|\.journal-[a-f0-9]{32}\.tmp)$/.exec(name);
      if (!match || this.entries.has(match[2])) continue;
      const path = join(this.root, name), info = lstatSync(path);
      if (this.now() - info.mtimeMs < DRAFT_TTL) continue;
      const fd = this.openRegistered(path, name.startsWith(".journal-") ? 16 * 1024 * 1024 : IMAGE_FILE_LIMIT); closeSync(fd); unlinkSync(path);
    }
    if (changed) this.save();
  }
}
