import { randomUUID } from "node:crypto";
import type { HarnessRuntime } from "./runtime.js";
import type { QueuedPrompt } from "./model.js";

export type PromptMode = "steer" | "queue";
export class InvalidPromptMode extends Error {}

/** Server-owned queue; a dispatch lock also covers asynchronous harness.prompt calls. */
export class PromptQueue {
  readonly entries: QueuedPrompt[] = [];
  private dispatching = false;
  constructor(private runtime: HarnessRuntime, private changed: () => void) {
    runtime.on("state", () => { void this.drain(); });
  }

  async submit(text: string, mode: PromptMode = "steer") {
    const snapshot = this.runtime.snapshot();
    if (!snapshot.ready) throw new Error("Harness is not ready");
    const busy = this.dispatching || snapshot.thread?.status === "running";
    const downgraded = mode === "steer" && !this.runtime.harness.capabilities.steer;
    const effectiveMode = downgraded ? "queue" : mode;
    if (busy && effectiveMode === "steer") {
      await this.runtime.prompt(text);
      return { disposition: "steered", mode: effectiveMode, downgraded: false, id: undefined };
    }
    const entry = { id: randomUUID(), text, createdAt: new Date().toISOString() };
    this.entries.push(entry);
    this.changed();
    // Dispatch idle prompts synchronously enough to surface startup/transport errors.
    if (!busy && this.entries.length === 1) await this.drain(true);
    return { disposition: this.entries.includes(entry) ? "queued" : "sent", mode: effectiveMode, id: entry.id,
      downgraded, ...(downgraded ? { explanation: "This harness does not support steering; using queue" } : {}) };
  }

  remove(id: string) {
    const index = this.entries.findIndex((entry) => entry.id === id);
    if (index < 0) return false;
    this.entries.splice(index, 1);
    this.changed();
    return true;
  }

  private async drain(propagate = false) {
    if (this.dispatching || !this.entries.length) return;
    const snapshot = this.runtime.snapshot();
    if (!snapshot.ready || snapshot.thread?.status !== "idle") return;
    this.dispatching = true;
    const entry = this.entries.shift()!;
    this.changed();
    try {
      await this.runtime.prompt(entry.text);
    } catch (error) {
      // Keep the message visible for deletion/retry, but never spin on a failure.
      this.entries.unshift(entry);
      this.changed();
      if (propagate) throw error;
      this.runtime.emit("log", `Queued prompt failed: ${error instanceof Error ? error.message : error}`);
      return;
    } finally { this.dispatching = false; }
    queueMicrotask(() => { void this.drain(); });
  }
}

export function promptMode(value: unknown): PromptMode {
  if (value === undefined) return "steer";
  if (value === "steer" || value === "queue") return value;
  throw new InvalidPromptMode("Delivery must be steer or queue");
}
