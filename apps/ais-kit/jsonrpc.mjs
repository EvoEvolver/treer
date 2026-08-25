import { EventEmitter } from "node:events";
import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";

export function createJsonRpcClient(options = {}) {
  const {
    stdin,
    stdout,
    includeJsonrpc = false,
    stringIds = false,
  } = options;
  const pending = new Map();
  const events = new EventEmitter();
  let nextId = 1;
  let closed = false;

  const reader = createInterface({ input: stdout });
  reader.on("line", (line) => {
    const trimmed = line.trim();
    if (!trimmed) return;
    let message;
    try {
      message = JSON.parse(trimmed);
    } catch {
      return;
    }
    if (message && Object.hasOwn(message, "id") && pending.has(message.id)) {
      const { resolve, reject } = pending.get(message.id);
      pending.delete(message.id);
      if (message.error) {
        reject(Object.assign(new Error(message.error.message ?? "JSON-RPC error"), {
          code: message.error.code,
          data: message.error.data,
        }));
        return;
      }
      resolve(message.result);
      return;
    }
    if (typeof message?.method === "string") {
      const params = message.params ?? {};
      events.emit("notification", message.method, params);
      // Node's EventEmitter treats "error" as fatal unless a listener exists.
      events.emit(message.method === "error" ? "server-error" : message.method, params);
    }
  });
  reader.on("close", () => {
    closed = true;
    for (const { reject } of pending.values()) {
      reject(new Error("JSON-RPC transport closed"));
    }
    pending.clear();
  });

  function write(message) {
    if (closed) throw new Error("JSON-RPC transport closed");
    stdin.write(`${JSON.stringify(message)}\n`);
  }

  return {
    events,
    request(method, params) {
      const id = stringIds ? `req_${randomUUID().replaceAll("-", "")}` : nextId++;
      const message = { method, id };
      if (includeJsonrpc) message.jsonrpc = "2.0";
      if (params !== undefined) message.params = params;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        write(message);
      });
    },
    notify(method, params) {
      const message = { method };
      if (includeJsonrpc) message.jsonrpc = "2.0";
      if (params !== undefined) message.params = params;
      write(message);
    },
    close() {
      closed = true;
      reader.close();
    },
  };
}
