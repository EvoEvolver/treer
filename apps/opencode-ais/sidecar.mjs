import { createServer } from "node:net";
import { spawn } from "node:child_process";
import { pathToFileURL } from "node:url";

import { mergeProviderEnv, newInstanceId, runRegisteredAis } from "../ais-kit/index.mjs";
import { createOpenCodeBackend } from "./backend.mjs";

async function allocateLoopbackPort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

export async function startOpenCodeAis(options = {}) {
  const env = await mergeProviderEnv(options.env ?? process.env);
  let child = options.child;
  let baseUrl = options.baseUrl;
  if (!baseUrl && !options.backend && !options.http) {
    const port = options.backendPort ?? await allocateLoopbackPort();
    baseUrl = `http://127.0.0.1:${port}`;
    child = child ?? spawn(options.command ?? "opencode", ["serve", "--hostname", "127.0.0.1", "--port", String(port)], {
      stdio: ["ignore", "pipe", "inherit"],
      env,
      cwd: options.cwd ?? process.cwd(),
    });
    for (let attempt = 0; attempt < 80; attempt += 1) {
      try {
        const health = await fetch(`${baseUrl}/global/health`);
        if (health.ok) break;
      } catch {
        await new Promise((resolve) => setTimeout(resolve, 150));
      }
    }
  }
  const http = options.http ?? ((path, init) => fetch(`${baseUrl}${path}`, init));
  const backend = options.backend ?? createOpenCodeBackend(http, {
    baseUrl,
    model: options.model ?? env.AIS_MODEL ?? env.MODEL,
    providerID: options.providerID ?? env.OPENCODE_PROVIDER ?? (env.OPENAI_BASE_URL ? "openai" : undefined),
  });
  await backend.start();
  const instanceId = options.instanceId ?? newInstanceId("opencode");
  return runRegisteredAis({
    instanceId,
    capabilities: backend.capabilities,
    getStatus: () => backend.status(),
    getEntries: () => backend.entries(),
    submitPrompt: ({ text }) => backend.prompt(text),
    abort: () => backend.abort(),
    stop: async () => {
      child?.kill?.("SIGTERM");
    },
  });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  await startOpenCodeAis();
}
