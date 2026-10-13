import { createHash } from "node:crypto";

export type Outcome = "done" | "failed" | "interrupted";
export interface PromptSender { agent_id: string; path: string; name: string }
export interface PromptNotify { on: Outcome[]; to?: string }
export interface Submission {
  id: string;
  sender: PromptSender | null;
  created_at: string;
  position: number | null;
  text_preview: string;
  state: "queued" | "running" | Outcome;
  turn_id: string | null;
  assistant_preview: string | null;
  error: string | null;
}
export interface Receipt { id: string; disposition: "queued" | "sent" | "steered"; position: number | null }
export interface SubmissionRecord {
  submission: Submission;
  operationId: string;
  fingerprint: string;
  text: string;
  delivery: "steer" | "queue";
  notify: PromptNotify | null;
  receipt: Receipt;
  notification: { attempts: number; nextAttempt: number; error: string | null; delivered: boolean } | null;
  priorTurnId?: string | null;
  sourceMessage?: { messageId: string; deliveryId: string; ackPending: boolean };
  /** Additional batch checkpoints; keep the first field readable by old journals. */
  sourceMessages?: { messageId: string; deliveryId: string; ackPending: boolean }[];
}
export const messageCheckpoints = (record: SubmissionRecord) => [...(record.sourceMessage ? [record.sourceMessage] : []), ...(record.sourceMessages ?? [])];
export class QueueError extends Error {
  constructor(message: string, readonly status = 400) { super(message); }
}
/** UTF-8 byte bound without splitting a codepoint. */
export function preview(text: string, max: number) {
  let bytes = 0, result = "";
  for (const char of text) { bytes += Buffer.byteLength(char); if (bytes > max) break; result += char; }
  return result;
}
export function submissionOptions(sender: unknown, notify: unknown): { sender: PromptSender | null; notify: PromptNotify | null } {
  let source: PromptSender | null = null;
  if (sender != null) {
    const value = sender as PromptSender;
    if (!value || typeof value !== "object" || [value.agent_id, value.path, value.name].some((part) => typeof part !== "string" || !part || part.length > 1024 || /[\r\n\u0000]/.test(part)) || !value.path.startsWith("/")) throw new QueueError("Invalid attested sender");
    source = { agent_id: value.agent_id, path: value.path, name: value.name };
  }
  let notification: PromptNotify | null = source ? { on: ["done", "failed", "interrupted"], to: source.agent_id } : null;
  if (notify != null) {
    const value = notify as PromptNotify;
    if (!value || typeof value !== "object" || Object.keys(value).some((key) => key !== "on" && key !== "to") || !Array.isArray(value.on) || value.on.some((outcome) => !["done", "failed", "interrupted"].includes(outcome)) || (value.to !== undefined && (typeof value.to !== "string" || !value.to || value.to.length > 1024 || /[\r\n\u0000]/.test(value.to)))) throw new QueueError("Invalid completion notification");
    notification = { on: [...new Set(value.on)].sort() as Outcome[], ...(value.to || source?.agent_id ? { to: value.to || source!.agent_id } : {}) };
    if (notification.on.length && !notification.to) throw new QueueError("Operator notifications require an explicit recipient");
  }
  return { sender: source, notify: notification };
}
export function fingerprint(text: string, delivery: string, sender: PromptSender | null, notify: PromptNotify | null) {
  return createHash("sha256").update(JSON.stringify({ text, delivery, sender, notify })).digest("hex");
}
