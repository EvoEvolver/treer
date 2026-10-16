import { randomUUID } from "node:crypto";
import { chmodSync, closeSync, constants, existsSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve, sep, parse, relative } from "node:path";
import { syncDirectory, protectDirectory } from "../scripts/private-state.mjs";
import { completionKey, submissionOptions, messageCheckpoints, type SubmissionRecord } from "./prompt-submission.js";
import { ATTACHMENT_ID, IMAGE_COUNT_LIMIT } from "./image-format.js";

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
      if (record.attachmentIds !== undefined && (!Array.isArray(record.attachmentIds) || !record.attachmentIds.length || record.attachmentIds.length > IMAGE_COUNT_LIMIT || record.attachmentIds.some(id => typeof id !== "string" || !ATTACHMENT_ID.test(id)) || new Set(record.attachmentIds).size !== record.attachmentIds.length || typeof record.attachmentSession !== "string" || !record.attachmentSession || record.attachmentSession.length > 256)) throw new Error("Invalid queue attachment metadata");
      if (!/^[a-f0-9]{64}$/.test(record.fingerprint) || !["steer", "queue"].includes(record.delivery) || !Number.isFinite(Date.parse(submission.created_at)) || typeof submission.text_preview !== "string" || Buffer.byteLength(submission.text_preview) > 1024 || Buffer.byteLength(record.text) > 65536 || !(submission.turn_id === null || typeof submission.turn_id === "string") || !(submission.error === null || typeof submission.error === "string") || !(submission.assistant_preview === null || typeof submission.assistant_preview === "string" && Buffer.byteLength(submission.assistant_preview) <= 4096) || record.receipt.id !== submission.id || !["queued", "sent", "steered"].includes(record.receipt.disposition)) throw new Error("Invalid queue journal metadata");
      submissionOptions(submission.sender, record.notify);
      if (record.turnScope !== undefined && (typeof record.turnScope !== "string" || !/^[a-f0-9-]{36}$/.test(record.turnScope))) throw new Error("Invalid queue turn scope");
      if (record.sourceMessages !== undefined && (!Array.isArray(record.sourceMessages) || record.sourceMessages.length > 255)) throw new Error("Invalid message acknowledgment batch");
      for (const message of messageCheckpoints(record)) if (!message || typeof message.ackPending !== "boolean" || [message.messageId, message.deliveryId].some((id) => typeof id !== "string" || !/^[a-zA-Z0-9_-]{1,256}$/.test(id))) throw new Error("Invalid message acknowledgment checkpoint");
      if (notification !== null && (!notification || typeof notification.delivered !== "boolean" || !Number.isInteger(notification.attempts) || notification.attempts < 0 || !Number.isFinite(notification.nextAttempt) || !(notification.error === null || typeof notification.error === "string"))) throw new Error("Invalid queue notification outbox");
      const batch = notification?.batch;
      if (batch !== undefined && (!batch || batch.key !== completionKey(record) || batch.recipient !== record.notify?.to || batch.turn_id !== submission.turn_id || !["done", "failed", "interrupted"].includes(batch.status) || !Array.isArray(batch.submission_ids) || !batch.submission_ids.length || batch.submission_ids.length > 8192 || batch.submission_ids.some((id) => typeof id !== "string" || !id || id.length > 1024) || new Set(batch.submission_ids).size !== batch.submission_ids.length || !batch.submission_ids.includes(submission.id) || !(batch.assistant_preview === null || typeof batch.assistant_preview === "string" && Buffer.byteLength(batch.assistant_preview) <= 4096) || !(batch.error === null || typeof batch.error === "string"))) throw new Error("Invalid queue completion batch");
      ids.add(record.submission.id); operations.add(record.operationId);
    }
    const records = journal.records as SubmissionRecord[], batches = new Set<string>();
    for (const record of records) if (record.notification?.batch && !record.notification.delivered) {
      const batch = record.notification.batch;
      if (batches.has(batch.key) || batch.submission_ids.some((id) => !records.some((member) => member.submission.id === id && member.notification && completionKey(member) === batch.key && !["queued", "running"].includes(member.submission.state)))) throw new Error("Invalid queue completion membership");
      batches.add(batch.key);
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
