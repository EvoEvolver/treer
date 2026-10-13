import { spawn } from "node:child_process";
import type { SubmissionRecord } from "./prompt-submission.js";

/** Uses this process's inherited Treer identity, never a sender's credentials. */
export function notifySubmission(record: SubmissionRecord): Promise<void> {
  const recipient = record.notify?.to;
  if (!recipient) return Promise.reject(new Error("Notification recipient is missing"));
  const submission = record.submission;
  const body = JSON.stringify({ type: "treer.prompt.completed", status: submission.state, submission_id: submission.id, turn_id: submission.turn_id, assistant_preview: submission.assistant_preview, error: submission.error });
  return sendNotification(recipient, `prompt-submission/${submission.id}/terminal`, body);
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
