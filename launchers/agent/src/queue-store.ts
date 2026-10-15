import { randomUUID } from "node:crypto";
import { chmodSync, closeSync, constants, existsSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve, sep, parse, relative } from "node:path";
import { syncDirectory, protectDirectory } from "../scripts/private-state.mjs";
import { submissionOptions, messageCheckpoints, type SubmissionRecord } from "./prompt-submission.js";

/** Small bounded journal; a commit completes before HTTP accepts a submission. */
export class QueueStore {
  constructor(readonly file: string) {
    const directory = dirname(resolve(file));
    // Queue state must not be redirected into credentials or another Agent's state.
    let current = parse(directory).root;
    for (const part of relative(current, directory).split(sep).filter(Boolean)) {
      current = join(current, part);
      if (existsSync(current)) {
        if (lstatSync(current).isSymbolicLink() || !lstatSync(current).isDirectory()) throw new Error("Queue storage rejects symlink/non-directory ancestors");
      } else {
        mkdirSync(current, { mode: 0o700 });
        syncDirectory(dirname(current));
      }
    }
    protectDirectory(directory);
  }
  load(): SubmissionRecord[] {
    if (!existsSync(this.file)) return [];
    const info = lstatSync(this.file);
    if (!info.isFile() || info.isSymbolicLink() || info.size > 96 * 1024 * 1024) throw new Error("Invalid queue journal file");
    chmodSync(this.file, 0o600);
    const data: unknown = JSON.parse(readFileSync(this.file, "utf8"));
    const journal = data as { version?: unknown; records?: unknown };
    if (journal?.version !== 1 || !Array.isArray(journal.records) || journal.records.length > 8192) throw new Error("Invalid queue journal; refusing to discard pending work");
    const ids = new Set<string>(), operations = new Set<string>();
    for (const record of journal.records as SubmissionRecord[]) {
      if (!record?.submission || typeof record.submission.id !== "string" || typeof record.operationId !== "string" || typeof record.text !== "string" || !record.receipt || !["queued", "running", "done", "failed", "interrupted"].includes(record.submission.state) || ids.has(record.submission.id) || operations.has(record.operationId)) throw new Error("Invalid queue journal entry; refusing to discard pending work");
      const submission = record.submission, notification = record.notification;
      if (!/^[a-f0-9]{64}$/.test(record.fingerprint) || !["steer", "queue"].includes(record.delivery) || !Number.isFinite(Date.parse(submission.created_at)) || typeof submission.text_preview !== "string" || Buffer.byteLength(submission.text_preview) > 1024 || Buffer.byteLength(record.text) > 65536 || !(submission.turn_id === null || typeof submission.turn_id === "string") || !(submission.error === null || typeof submission.error === "string") || !(submission.assistant_preview === null || typeof submission.assistant_preview === "string" && Buffer.byteLength(submission.assistant_preview) <= 4096) || record.receipt.id !== submission.id || !["queued", "sent", "steered"].includes(record.receipt.disposition)) throw new Error("Invalid queue journal metadata");
      submissionOptions(submission.sender, record.notify);
      if (record.sourceMessages !== undefined && (!Array.isArray(record.sourceMessages) || record.sourceMessages.length > 255)) throw new Error("Invalid message acknowledgment batch");
      for (const message of messageCheckpoints(record)) if (!message || typeof message.ackPending !== "boolean" || [message.messageId, message.deliveryId].some((id) => typeof id !== "string" || !/^[a-zA-Z0-9_-]{1,256}$/.test(id))) throw new Error("Invalid message acknowledgment checkpoint");
      if (notification !== null && (!notification || typeof notification.delivered !== "boolean" || !Number.isInteger(notification.attempts) || notification.attempts < 0 || !Number.isFinite(notification.nextAttempt) || !(notification.error === null || typeof notification.error === "string"))) throw new Error("Invalid queue notification outbox");
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
      syncDirectory(directory);
    } finally {
      if (handle !== undefined) closeSync(handle);
      if (existsSync(temporary)) unlinkSync(temporary);
    }
  }
}
