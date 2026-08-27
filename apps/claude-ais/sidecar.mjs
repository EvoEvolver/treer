import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

import { mergeProviderEnv, newInstanceId, runRegisteredAis } from "../ais-kit/index.mjs";
import { createClaudeBackend } from "./backend.mjs";

export function createClaudeStreamIo(child) {
  const listeners = new Set();
  const reader = createInterface({ input: child.stdout });
  reader.on("line", (line) => {
    const trimmed = line.trim();
    if (!trimmed) return;
    try {
      const event = JSON.parse(trimmed);
      for (const listener of listeners) listener(event);
    } catch {
      // Ignore non-JSON diagnostic lines.
    }
  });
  return {
    onEvent(listener) {
      listeners.add(listener);
    },
    async send(message) {
      child.stdin.write(`${JSON.stringify(message)}\n`);
    },
  };
}

export async function startClaudeAis(options = {}) {
  const env = await mergeProviderEnv(options.env ?? process.env);
  const model = options.model ?? env.CLAUDE_MODEL ?? env.AIS_MODEL ?? env.MODEL;
  if (!model) {
    delete env.ANTHROPIC_API_KEY;
    delete env.ANTHROPIC_AUTH_TOKEN;
    delete env.ANTHROPIC_BASE_URL;
  }
  const args = options.args ?? [
    "--print",
    "--verbose",
    "--output-format", "stream-json",
    "--input-format", "stream-json",
    "--dangerously-skip-permissions",
    ...(model ? ["--model", model] : []),
  ];
  const child = options.child ?? (options.io || options.backend
    ? { kill() {} }
    : spawn(options.command ?? "claude", args, {
        stdio: ["pipe", "pipe", "inherit"],
        env,
        cwd: options.cwd ?? process.cwd(),
      }));
  const io = options.io ?? (child.stdout ? createClaudeStreamIo(child) : {
    onEvent() {},
    async send() {},
  });
  const backend = options.backend ?? createClaudeBackend(io);
  await backend.start();
  const instanceId = options.instanceId ?? newInstanceId("claude");
  return runRegisteredAis({
    instanceId,
    capabilities: backend.capabilities,
    getStatus: () => backend.status(),
    getEntries: () => backend.entries(),
    submitPrompt: ({ text }) => backend.prompt(text),
    abort: () => backend.abort(),
    stop: async () => {
      child.kill?.("SIGTERM");
    },
  });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  await startClaudeAis();
}
