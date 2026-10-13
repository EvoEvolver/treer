import { randomUUID } from "node:crypto";
import type { HarnessRuntime } from "./runtime.js";
import type { QueuedPrompt } from "./model.js";
import { QueueStore } from "./queue-store.js";
import { fingerprint, preview, QueueError, submissionOptions, messageCheckpoints, type Outcome, type PromptNotify, type PromptSender, type SubmissionRecord } from "./prompt-submission.js";

export type PromptMode = "steer" | "queue";
export class InvalidPromptMode extends Error {}
interface QueueOptions {
  file?: string;
  limit?: number;
  notify?: (record: SubmissionRecord) => Promise<void>;
  retryMs?: number;
}
interface SubmitOptions { operationId?: string; sender?: PromptSender | null; notify?: PromptNotify | null; sourceMessage?: { messageId: string; deliveryId: string }; sourceMessages?: { messageId: string; deliveryId: string }[] }

/** One server-owned journal and turn correlator, shared by every harness. */
export class PromptQueue {
  private records: SubmissionRecord[] = [];
  private store?: QueueStore;
  private dispatching = false;
  private notifying = false;
  private retry?: ReturnType<typeof setTimeout>;
  private closed = false;
  private limit: number;
  constructor(private runtime: HarnessRuntime, private changed: () => void, private options: QueueOptions = {}) {
    this.limit = options.limit ?? 256;
    if (!Number.isInteger(this.limit) || this.limit < 1 || this.limit > 4096) throw new QueueError("Queue limit must be an integer from 1 to 4096");
    if (options.file) { this.store = new QueueStore(options.file); this.records = this.store.load(); }
    for (const record of this.records) if (record.submission.state === "running") this.finish(record, "interrupted", "Launcher restarted before the submitted turn was confirmed complete");
    this.persist();
    runtime.on("state", () => { this.observe(); void this.drain(); });
    runtime.on("exit", () => {
      for (const record of this.records) if (record.submission.state === "running") this.finish(record, "interrupted", "Harness process exited");
      this.persist(); this.changed(); void this.flushNotifications();
    });
    queueMicrotask(() => { void this.flushNotifications(); });
  }

  get entries(): QueuedPrompt[] {
    return this.records.filter((record) => record.submission.state === "queued").map((record, index) => ({ id: record.submission.id, text: record.text, createdAt: record.submission.created_at, sender: record.submission.sender, position: index + 1 }));
  }
  get busy() { return this.dispatching || this.runtime.snapshot().thread?.status === "running"; }
  get submissions() { return this.records.filter((record) => record.submission.state === "queued").map((record) => ({ ...record.submission })); }
  submission(id: string) { const record = this.records.find((entry) => entry.submission.id === id); return record ? { ...record.submission } : undefined; }
  close() { this.closed = true; if (this.retry) clearTimeout(this.retry); }

  pendingMessageAcks() {
    if (this.closed) throw new QueueError("Prompt queue is closed", 503);
    return this.records.flatMap((record) => messageCheckpoints(record).filter((message) => message.ackPending).map((message) => ({ operationId: record.operationId, ...message })));
  }
  hasMessage(messageId: string) {
    if (this.closed) throw new QueueError("Prompt queue is closed", 503);
    return this.records.some((record) => messageCheckpoints(record).some((message) => message.messageId === messageId));
  }
  acknowledgeMessage(operationId: string, deliveryId: string) {
    const record = this.records.find((entry) => entry.operationId === operationId);
    const message = record && messageCheckpoints(record).find((message) => message.deliveryId === deliveryId);
    if (!message) throw new QueueError("Unknown message acknowledgment");
    message.ackPending = false; this.persist();
  }

  private persist() {
    let position = 0;
    for (const record of this.records) record.submission.position = record.submission.state === "queued" ? ++position : null;
    let terminal = this.records.filter((record) => !["queued", "running"].includes(record.submission.state) && (!record.notification || record.notification.delivered)).length;
    this.records = this.records.filter((record) => {
      if (terminal > 1024 && !messageCheckpoints(record).some((message) => message.ackPending) && !["queued", "running"].includes(record.submission.state) && (!record.notification || record.notification.delivered)) { terminal--; return false; }
      return true;
    });
    try { this.store?.save(this.records); }
    catch (error) { this.closed = true; throw error; } // Fail closed: uncommitted work must never dispatch.
  }

  async submit(text: string, mode: PromptMode = "steer", options: SubmitOptions = {}) {
    if (!text.trim() || Buffer.byteLength(text) > 65536) throw new QueueError("Prompt must contain text and fit within 64 KiB");
    const operationId = options.operationId ?? randomUUID();
    if (!operationId || operationId.length > 1024) throw new QueueError("Invalid operation_id");
    const metadata = submissionOptions(options.sender, options.notify);
    const hash = fingerprint(text, mode, metadata.sender, metadata.notify);
    const existing = this.records.find((record) => record.operationId === operationId);
    if (existing) {
      if (existing.fingerprint !== hash) throw new QueueError("operation_id was already used for a different submission", 409);
      return { ...existing.receipt, duplicate: true, mode: existing.delivery, downgraded: mode !== existing.delivery };
    }
    if (!this.runtime.snapshot().ready) throw new QueueError("Harness is not ready", 503);
    if (this.closed) throw new QueueError("Prompt queue is closed after shutdown or a journal failure", 503);
    if (this.records.filter((record) => ["queued", "running"].includes(record.submission.state) || messageCheckpoints(record).some((message) => message.ackPending) || record.notification && !record.notification.delivered).length >= this.limit) throw new QueueError(`Prompt queue is full (limit ${this.limit}, including pending notifications and acknowledgments)`, 429);
    const busy = this.busy;
    const downgraded = mode === "steer" && !this.runtime.harness.capabilities.steer;
    const delivery = downgraded ? "queue" : mode;
    const steering = busy && delivery === "steer" && !this.dispatching;
    const id = `sub_${randomUUID()}`;
    const record: SubmissionRecord = {
      submission: { id, sender: metadata.sender, created_at: new Date().toISOString(), position: null, text_preview: preview(text, 1024), state: steering ? "running" : "queued", turn_id: steering ? this.runtime.snapshot().thread?.activeTurnId ?? null : null, assistant_preview: null, error: null },
      operationId, fingerprint: hash, text, delivery, notify: metadata.notify,
      receipt: { id, disposition: steering ? "steered" : busy || this.entries.length ? "queued" : "sent", position: null }, notification: null,
      ...(options.sourceMessage ? { sourceMessage: { ...options.sourceMessage, ackPending: true } } : {}),
      ...(options.sourceMessages ? { sourceMessages: options.sourceMessages.map((message) => ({ ...message, ackPending: true })) } : {}),
    };
    this.records.push(record);
    this.persist(); record.receipt.position = record.receipt.disposition === "queued" ? record.submission.position : null; this.persist();
    this.changed();
    if (steering) await this.dispatch(record);
    // Steering adapters acknowledge insertion before returning. ACP does not
    // support steering and resolves only at completion: do not wait for its turn.
    else if (!busy && this.runtime.harness.capabilities.steer && !options.sourceMessage) await this.drain();
    else if (!busy) void this.drain();
    return { ...record.receipt, duplicate: false, mode: delivery, downgraded, ...(downgraded ? { explanation: "This harness does not support steering; using queue" } : {}) };
  }

  remove(id: string) {
    const record = this.records.find((entry) => entry.submission.id === id);
    if (!record) return false;
    if (!["queued", "running"].includes(record.submission.state)) return false;
    if (record.submission.state !== "queued") throw new QueueError("Only pending submissions can be cancelled; abort the running turn separately", 409);
    this.finish(record, "interrupted", "Cancelled before dispatch");
    this.persist(); this.changed(); void this.flushNotifications();
    return true;
  }

  private async dispatch(record: SubmissionRecord) {
    record.priorTurnId = this.runtime.snapshot(10_000).thread?.turns?.at(-1)?.id ?? null;
    record.submission.state = "running"; record.submission.position = null;
    this.persist(); this.changed();
    const sender = record.submission.sender;
    const prefix = sender && !record.sourceMessage ? `[来自 ${sender.path}，排队于 ${record.submission.created_at}]\n` : "";
    try {
      await this.runtime.prompt(prefix + record.text);
      const thread = this.runtime.snapshot(10_000).thread;
      const latest = thread?.turns?.at(-1)?.id;
      record.submission.turn_id ??= thread?.activeTurnId ?? (latest && latest !== record.priorTurnId ? latest : null);
      this.persist();
    } catch (error) {
      this.finish(record, "failed", error instanceof Error ? error.message : String(error));
      this.persist(); this.changed();
      this.runtime.emit("log", `Submission ${record.submission.id} failed: ${record.submission.error}`);
    }
    this.observe();
  }

  private async drain() {
    if (this.closed || this.dispatching) return;
    const snapshot = this.runtime.snapshot();
    if (!snapshot.ready || snapshot.thread?.status !== "idle") return;
    const record = this.records.find((entry) => entry.submission.state === "queued");
    if (!record) return;
    this.dispatching = true;
    try { await this.dispatch(record); }
    finally { this.dispatching = false; }
    this.observe();
    queueMicrotask(() => { void this.drain(); });
  }

  private finish(record: SubmissionRecord, outcome: Outcome, error: string | null = null) {
    record.submission.state = outcome; record.submission.position = null; record.submission.error = error;
    record.text = ""; // Terminal receipts retain bounded previews, not another copy of every conversation.
    if (record.notify?.to && record.notify.on.includes(outcome)) record.notification = { attempts: 0, nextAttempt: 0, error: null, delivered: false };
  }
  private observe() {
    if (this.dispatching) return;
    const thread = this.runtime.snapshot(10_000).thread;
    let changed = false;
    for (const record of this.records) {
      if (record.submission.state !== "running") continue;
      const latest = thread?.turns?.at(-1)?.id;
      record.submission.turn_id ??= thread?.activeTurnId ?? (latest && latest !== record.priorTurnId ? latest : null);
      const turn = thread?.turns?.find((entry) => entry.id === record.submission.turn_id);
      if (turn && turn.status === "inProgress") continue;
      if (!turn && thread?.status === "running") continue;
      record.submission.assistant_preview = preview(turn?.items.filter((item) => item.kind === "agentMessage").at(-1)?.text ?? "", 4096) || null;
      const failed = thread?.status === "error" || turn?.status === "failed";
      this.finish(record, turn?.status === "interrupted" ? "interrupted" : failed ? "failed" : "done", failed ? thread?.lastError ?? "Turn failed" : null);
      changed = true;
    }
    if (changed) { this.persist(); this.changed(); }
    void this.flushNotifications();
  }

  async flushNotifications() {
    if (this.closed || this.notifying || !this.options.notify) return;
    this.notifying = true;
    try {
      for (const record of this.records) {
        const notification = record.notification;
        if (!notification || notification.delivered || notification.nextAttempt > Date.now()) continue;
        try { await this.options.notify(record); notification.delivered = true; notification.error = null; }
        catch (error) {
          notification.attempts++;
          notification.error = error instanceof Error ? error.message : String(error);
          notification.nextAttempt = Date.now() + Math.min(60_000, (this.options.retryMs ?? 1000) * 2 ** Math.min(6, notification.attempts - 1));
          this.runtime.emit("log", `Completion notification ${record.submission.id} failed; retrying: ${notification.error}`);
        }
        this.persist();
      }
    } finally {
      this.notifying = false;
      if (this.retry) clearTimeout(this.retry);
      const pending = this.records.filter((record) => record.notification && !record.notification.delivered);
      if (pending.length && !this.closed) {
        const next = Math.min(...pending.map((record) => record.notification!.nextAttempt));
        this.retry = setTimeout(() => { void this.flushNotifications(); }, Math.max(1, next - Date.now()));
        this.retry.unref();
      }
    }
  }
}

export function promptMode(value: unknown, fallback: PromptMode = "steer"): PromptMode {
  if (value === undefined) return fallback;
  if (value === "steer" || value === "queue") return value;
  throw new InvalidPromptMode("Delivery must be steer or queue");
}
