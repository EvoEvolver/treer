import { randomUUID } from "node:crypto";
import { chmodSync, closeSync, constants, existsSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import type { SubmissionRecord } from "./prompt-submission.js";

/** Small bounded journal; a commit completes before HTTP accepts a submission. */
export class QueueStore {
  constructor(readonly file: string) {
    const directory = dirname(resolve(file));
    // Queue state must not be redirected into credentials or another Agent's state.
    let current = directory.startsWith(sep) ? sep : "";
    for (const part of directory.split(sep).filter(Boolean)) {
      current = join(current, part);
      if (existsSync(current)) {
        if (lstatSync(current).isSymbolicLink() || !lstatSync(current).isDirectory()) throw new Error("Queue storage rejects symlink/non-directory ancestors");
      } else {
        mkdirSync(current, { mode: 0o700 });
        const parent = openSync(dirname(current), constants.O_RDONLY);
        try { fsyncSync(parent); } finally { closeSync(parent); }
      }
    }
  }
  load(): SubmissionRecord[] {
    if (!existsSync(this.file)) return [];
    const info = lstatSync(this.file);
    if (!info.isFile() || info.isSymbolicLink() || info.size > 96 * 1024 * 1024) throw new Error("Invalid queue journal file");
    chmodSync(this.file, 0o600);
    const data: unknown = JSON.parse(readFileSync(this.file, "utf8"));
    const journal = data as { version?: unknown; records?: unknown };
    if (journal?.version !== 1 || !Array.isArray(journal.records)) throw new Error("Invalid queue journal; refusing to discard pending work");
    const ids = new Set<string>(), operations = new Set<string>();
    for (const record of journal.records as SubmissionRecord[]) {
      if (!record?.submission || typeof record.submission.id !== "string" || typeof record.operationId !== "string" || typeof record.text !== "string" || !record.receipt || !["queued", "running", "done", "failed", "interrupted"].includes(record.submission.state) || ids.has(record.submission.id) || operations.has(record.operationId)) throw new Error("Invalid queue journal entry; refusing to discard pending work");
      ids.add(record.submission.id); operations.add(record.operationId);
    }
    return journal.records;
  }
  save(records: SubmissionRecord[]) {
    const directory = dirname(this.file), temporary = join(directory, `.queue-${randomUUID()}.tmp`);
    let handle: number | undefined;
    try {
      handle = openSync(temporary, constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY, 0o600);
      writeFileSync(handle, JSON.stringify({ version: 1, records }));
      fsyncSync(handle); closeSync(handle); handle = undefined;
      renameSync(temporary, this.file);
      const parent = openSync(directory, constants.O_RDONLY);
      try { fsyncSync(parent); } finally { closeSync(parent); }
    } finally {
      if (handle !== undefined) closeSync(handle);
      if (existsSync(temporary)) unlinkSync(temporary);
    }
  }
}
