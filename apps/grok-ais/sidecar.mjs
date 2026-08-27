import { spawn } from "node:child_process";
import { pathToFileURL } from "node:url";

import {
  createAcpBackend,
  createJsonRpcClient,
  newInstanceId,
  runRegisteredAis,
  selectAuthMethod,
} from "../ais-kit/index.mjs";

export function grokAcpArgs(env = process.env, options = {}) {
  const model = options.model ?? env.GROK_AIS_MODEL ?? env.GROK_MODEL;
  return [
    "--no-auto-update",
    "agent",
    "--always-approve",
    ...(model ? ["--model", model] : []),
    "stdio",
  ];
}

export async function startGrokAis(options = {}) {
  const env = options.env ?? process.env;
  const child = options.child ?? (options.backend || options.rpc
    ? { kill() {} }
    : spawn(options.command ?? "grok", options.args ?? grokAcpArgs(env, options), {
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
    clientInfo: { name: "treer-grok-ais", version: "0.1.0" },
    selectAuth: (methods) => selectAuthMethod(methods, [
      env.XAI_API_KEY ? "xai.api_key" : null,
      "cached_token",
    ].filter(Boolean)),
    authParams: { _meta: { headless: true } },
  });
  await backend.start();
  const instanceId = options.instanceId ?? newInstanceId("grok");
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
  await startGrokAis();
}
