import { spawn } from "node:child_process";
import { pathToFileURL } from "node:url";

import {
  createAcpBackend,
  createJsonRpcClient,
  newInstanceId,
  runRegisteredAis,
  selectAuthMethod,
} from "../ais-kit/index.mjs";

export function cursorAcpArgs(env = process.env, options = {}) {
  const model = options.model ?? env.CURSOR_AIS_MODEL ?? env.CURSOR_MODEL;
  return [
    "--trust",
    "--force",
    "--sandbox", "disabled",
    ...(model ? ["--model", model] : []),
    "acp",
  ];
}

export async function startCursorAis(options = {}) {
  const env = options.env ?? process.env;
  const child = options.child ?? (options.backend || options.rpc
    ? { kill() {} }
    : spawn(options.command ?? "cursor-agent", options.args ?? cursorAcpArgs(env, options), {
        stdio: ["pipe", "pipe", "inherit"],
        env,
        cwd: options.cwd ?? process.cwd(),
      }));
  const rpc = options.rpc ?? (child.stdin && child.stdout
    ? createJsonRpcClient({
        stdin: child.stdin,
        stdout: child.stdout,
        includeJsonrpc: true,
        stringIds: true,
      })
    : {
        events: { on() {} },
        request: async () => ({}),
        notify() {},
        respond() {},
        close() {},
      });
  const backend = options.backend ?? createAcpBackend(rpc, {
    cwd: options.cwd ?? process.cwd(),
    clientInfo: { name: "treer-cursor-ais", version: "0.1.0" },
    selectAuth: (methods) => selectAuthMethod(methods, ["cursor_login"]),
  });
  await backend.start();
  const instanceId = options.instanceId ?? newInstanceId("cursor");
  return runRegisteredAis({
    instanceId,
    capabilities: backend.capabilities,
    getStatus: () => backend.status(),
    getEntries: () => backend.entries(),
    submitPrompt: ({ text }) => backend.prompt(text),
    abort: () => backend.abort(),
    stop: async () => {
      rpc.close?.();
      child.kill?.("SIGTERM");
    },
  });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  await startCursorAis();
}
