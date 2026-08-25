import { spawn } from "node:child_process";
import { pathToFileURL } from "node:url";

import { createJsonRpcClient, mergeProviderEnv, newInstanceId, runRegisteredAis } from "../ais-kit/index.mjs";
import { createCodexBackend } from "./backend.mjs";

export async function startCodexAis(options = {}) {
  const env = await mergeProviderEnv(options.env ?? process.env);
  const child = options.child ?? (options.backend || options.rpc
    ? { kill() {} }
    : spawn(options.command ?? "codex", options.args ?? ["app-server"], {
        stdio: ["pipe", "pipe", "inherit"],
        env,
        cwd: options.cwd ?? process.cwd(),
      }));
  const rpc = options.rpc ?? (child.stdin && child.stdout
    ? createJsonRpcClient({ stdin: child.stdin, stdout: child.stdout })
    : { events: { on() {} }, request: async () => ({}), notify() {}, close() {} });
  const backend = options.backend ?? createCodexBackend(rpc, {
    cwd: options.cwd ?? process.cwd(),
    model: options.model ?? env.CODEX_AIS_MODEL ?? env.AIS_MODEL ?? env.MODEL,
  });
  await backend.start();
  const instanceId = options.instanceId ?? newInstanceId("codex");
  return runRegisteredAis({
    instanceId,
    capabilities: backend.capabilities,
    getStatus: () => backend.status(),
    getEntries: () => backend.entries(),
    submitPrompt: ({ text }) => backend.prompt(text),
    abort: () => backend.abort(),
    stop: async () => {
      rpc.close();
      child.kill?.("SIGTERM");
    },
  });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  await startCodexAis();
}
