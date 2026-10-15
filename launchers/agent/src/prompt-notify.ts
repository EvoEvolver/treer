import { spawn } from "node:child_process";
import { completionKey, preview, type SubmissionRecord } from "./prompt-submission.js";

/** Uses this process's inherited Treer identity, never a sender's credentials. */
export async function notifySubmission(record: SubmissionRecord): Promise<void> {
  const { recipient, key, body } = submissionNotification(record);
  await sendNotification(recipient, key, body);
}

export function submissionNotification(record: SubmissionRecord) {
  const recipient = record.notify?.to;
  if (!recipient) throw new Error("Notification recipient is missing");
  const submission = record.submission;
  const batch = record.notification?.batch;
  const body = { type: "treer.prompt.completed", status: batch?.status ?? submission.state,
    submission_id: batch?.submission_ids[0] ?? submission.id, submission_ids: batch?.submission_ids ?? [submission.id],
    turn_id: batch?.turn_id ?? submission.turn_id, assistant_preview: batch ? batch.assistant_preview : submission.assistant_preview, error: preview((batch ? batch.error : submission.error) ?? "", 1024) || null };
  // JSON escaping can expand control characters sixfold. Keep every ID; only
  // shorten bounded diagnostic previews, never silently split the completion.
  while (Buffer.byteLength(JSON.stringify(body)) > 32768 && (body.assistant_preview || body.error)) {
    if (body.assistant_preview) body.assistant_preview = preview(body.assistant_preview, Math.floor(Buffer.byteLength(body.assistant_preview) / 2)) || null;
    else if (body.error) body.error = preview(body.error, Math.floor(Buffer.byteLength(body.error) / 2)) || null;
  }
  if (Buffer.byteLength(JSON.stringify(body)) > 32768) throw new Error("Completion membership exceeds the Core Message limit; retained in the notification outbox");
  return { recipient, key: batch?.key ?? completionKey(record), body: JSON.stringify(body) };
}

export function sendNotification(recipient: string, key: string, body: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const child = spawn("treer", ["message", "send", "--to", recipient, "--idempotency-key", key], { stdio: ["pipe", "pipe", "pipe"] });
    let outputBytes = 0;
    const fail = (error: Error) => { clearTimeout(timer); child.kill("SIGKILL"); reject(error); };
    const timer = setTimeout(() => fail(new Error("Treer completion notification timed out")), 15_000);
    timer.unref();
    // Drain output, but never log CLI response bodies or credentials.
    const drain = (chunk: Buffer) => { outputBytes += chunk.length; if (outputBytes > 65536) fail(new Error("Treer completion notification exceeded output limit")); };
    child.stdout.on("data", drain); child.stderr.on("data", drain);
    child.on("error", (error) => { clearTimeout(timer); reject(error); });
    child.on("close", (code) => { clearTimeout(timer); if (code === 0) resolve(); else reject(new Error(`Treer completion notification exited with code ${code}`)); });
    child.stdin.on("error", fail);
    child.stdin.end(body + "\n");
  });
}
