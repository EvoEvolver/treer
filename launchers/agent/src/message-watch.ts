import { spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import type { PromptQueue } from "./prompt-queue.js";
import { messagePrompt, messageSection, validMessageId as identifier, mergeMessagePrompts } from "./message-prompt.js";
export { MESSAGE_WARNING } from "./message-prompt.js";

export interface MessageDelivery {
  delivery_id: string;
  recipient: { kind: string; id: string };
  message: { message_id: string; sender: { kind: string; id: string; path?: string | null; name: string }; body: string; created_at: string };
}
export function wrapMessage(message: MessageDelivery["message"]) {
  return messagePrompt([messageSection(message.sender.path || message.sender.name || message.sender.id, message.body)], [message.message_id]);
}

/** Own inherited Agent credentials only; no shell interpolation or message contents in logs. */
export function messageCommand(args: string[], signal: AbortSignal): Promise<unknown> {
  return new Promise((resolve, reject) => {
    const child = spawn("treer", ["message", ...args], { stdio: ["ignore", "pipe", "pipe"], signal });
    const chunks: Buffer[] = []; let size = 0, exceeded = false;
    const timeout = setTimeout(() => child.kill("SIGKILL"), 45_000);
    child.stdout.on("data", (chunk: Buffer) => {
      size += chunk.length;
      if (size > 16 * 1024 * 1024) { exceeded = true; child.kill("SIGKILL"); }
      else chunks.push(chunk);
    });
    child.stderr.resume();
    child.on("error", reject);
    child.on("close", (code) => {
      clearTimeout(timeout);
      if (code !== 0 || exceeded) reject(new Error(`Treer message ${args[0]} failed (${exceeded ? "response too large" : code})`));
      else {
        try { const text = Buffer.concat(chunks).toString("utf8"); resolve(text.trim() ? JSON.parse(text) : null); }
        catch { reject(new Error("Invalid Treer message JSON response")); }
      }
    });
  });
}

export function messageWatchEnabled(env: NodeJS.ProcessEnv) {
  return !["0", "false", "off"].includes((env.AGENT_UI_MESSAGE_WATCH ?? "").toLowerCase()) && Boolean(env.TREER_AGENT_ID && env.TREER_WORKLOAD_CREDENTIAL && env.TREER_AGENT_SERVER_URL);
}

export class MessageWatch {
  private abort = new AbortController();
  private task?: Promise<void>;
  constructor(private queue: PromptQueue, private agentId: string, private log: (message: string) => void, private command = messageCommand, private retryMs = 1000) {}
  start() { this.task ??= this.run(); }
  async stop() { this.abort.abort(); await this.task; }
  private async ack(deliveryId: string) {
    await this.command(["ack", deliveryId, "--operation-id", `launcher-message-ack/${deliveryId}`], this.abort.signal);
  }
  async receive(delivery: MessageDelivery) {
    await this.receiveBatch([delivery]);
  }
  async receiveBatch(deliveries: MessageDelivery[]) {
    if (deliveries.length > 10) throw new Error("Invalid message batch size");
    for (const delivery of deliveries) {
      if (!identifier(delivery?.delivery_id) || !identifier(delivery?.message?.message_id) || delivery.recipient?.kind !== "agent" || delivery.recipient.id !== this.agentId || typeof delivery.message.body !== "string" || !delivery.message.sender || typeof delivery.message.sender.name !== "string" || typeof delivery.message.sender.id !== "string" || !(delivery.message.sender.path == null || typeof delivery.message.sender.path === "string")) throw new Error("Invalid message or recipient identity");
    }
    const fresh = [...new Map(deliveries.filter((delivery) => !this.queue.hasMessage(delivery.message.message_id)).map((delivery) => [delivery.message.message_id, delivery])).values()];
    // Pack before submitting so an idle harness receives the complete poll in
    // one turn too. Subsequent polls coalesce at the queue tail while it is busy.
    const batches: { text: string; checkpoints: { messageId: string; deliveryId: string }[] }[] = [];
    for (const delivery of fresh) {
      const checkpoint = { messageId: delivery.message.message_id, deliveryId: delivery.delivery_id };
      const text = wrapMessage(delivery.message), last = batches.at(-1);
      const merged = last && mergeMessagePrompts(last.text, last.checkpoints.map((entry) => entry.messageId), text, [checkpoint.messageId]);
      if (last && merged) { last.text = merged; last.checkpoints.push(checkpoint); }
      else batches.push({ text, checkpoints: [checkpoint] });
    }
    for (const { text, checkpoints } of batches) {
      await this.queue.submit(text, "queue", {
        operationId: `treer-message/${checkpoints[0].messageId}`, notify: { on: [] }, sender: null,
        sourceMessage: checkpoints[0], sourceMessages: checkpoints.slice(1), coalesceMessages: true,
      });
    }
    for (const delivery of deliveries) {
      await this.ack(delivery.delivery_id);
      const pending = this.queue.pendingMessageAcks().find((entry) => entry.deliveryId === delivery.delivery_id);
      if (pending) this.queue.acknowledgeMessage(pending.operationId, delivery.delivery_id);
    }
  }
  private async run() {
    let failures = 0;
    while (!this.abort.signal.aborted) {
      try {
        // Retry even when the remote ack succeeded just before a local crash.
        for (const entry of this.queue.pendingMessageAcks()) {
          await this.ack(entry.deliveryId);
          this.queue.acknowledgeMessage(entry.operationId, entry.deliveryId);
        }
        const result = await this.command(["receive", "--wait", "30000", "--limit", "10"], this.abort.signal) as { deliveries?: MessageDelivery[] };
        if (!result || !Array.isArray(result.deliveries)) throw new Error("Invalid message receive response");
        await this.receiveBatch(result.deliveries);
        failures = 0;
        if (!result.deliveries.length) await delay(250, undefined, { signal: this.abort.signal });
      } catch (error) {
        if (this.abort.signal.aborted) break;
        this.log(`Message watcher failed; retrying: ${error instanceof Error ? error.message : "unknown error"}`);
        try { await delay(Math.min(60_000, this.retryMs * 2 ** Math.min(failures++, 6)), undefined, { signal: this.abort.signal }); }
        catch { break; }
      }
    }
  }
}
