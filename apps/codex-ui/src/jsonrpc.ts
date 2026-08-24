import { EventEmitter } from "node:events";
import readline from "node:readline";
import type { Readable, Writable } from "node:stream";

type JsonRpcId = number;

interface Pending {
  resolve: (value: unknown) => void;
  reject: (reason?: unknown) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class JsonRpcClient extends EventEmitter {
  private readonly reader: readline.Interface;
  private readonly pending = new Map<JsonRpcId, Pending>();
  private nextId = 1;
  private closed = false;

  constructor(
    input: Readable,
    private readonly output: Writable,
  ) {
    super();
    this.reader = readline.createInterface({ input, crlfDelay: Infinity });
    this.reader.on("line", (line) => this.handle(line));
    this.reader.on("close", () => this.close());
  }

  async request<T = unknown>(method: string, params?: unknown, timeoutMs = 30_000): Promise<T> {
    if (this.closed) {
      throw new Error("JSON-RPC client is closed");
    }
    const id = this.nextId++;
    const payload: Record<string, unknown> = { jsonrpc: "2.0", id, method };
    if (params !== undefined) {
      payload.params = params;
    }
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`JSON-RPC timeout for ${method}`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (value) => resolve(value as T),
        reject,
        timer,
      });
      this.output.write(`${JSON.stringify(payload)}\n`);
    });
  }

  respond(id: JsonRpcId, result: unknown) {
    this.output.write(`${JSON.stringify({ jsonrpc: "2.0", id, result })}\n`);
  }

  close() {
    if (this.closed) {
      return;
    }
    this.closed = true;
    this.reader.close();
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(new Error("JSON-RPC client closed"));
    }
    this.pending.clear();
  }

  private handle(raw: string) {
    if (!raw.trim()) {
      return;
    }
    let parsed: Record<string, unknown>;
    try {
      parsed = JSON.parse(raw) as Record<string, unknown>;
    } catch {
      return;
    }
    if (typeof parsed.method === "string" && !("id" in parsed)) {
      this.emit("notification", parsed);
      return;
    }
    if (typeof parsed.method === "string" && typeof parsed.id === "number") {
      this.emit("request", parsed);
      return;
    }
    if (typeof parsed.id !== "number") {
      return;
    }
    const pending = this.pending.get(parsed.id);
    if (!pending) {
      return;
    }
    clearTimeout(pending.timer);
    this.pending.delete(parsed.id);
    if (parsed.error && typeof parsed.error === "object") {
      const error = parsed.error as { message?: string };
      pending.reject(new Error(error.message ?? "JSON-RPC error"));
      return;
    }
    pending.resolve(parsed.result);
  }
}
