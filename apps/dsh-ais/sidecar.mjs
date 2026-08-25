import { createServer } from "node:net";
import { spawn } from "node:child_process";
import { pathToFileURL } from "node:url";

import { createJsonRpcClient, mergeProviderEnv, newInstanceId, runRegisteredAis } from "../ais-kit/index.mjs";
import { createDshHostBackend, createDshSdkBackend } from "./backend.mjs";

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

export async function startDshAis(options = {}) {
  const env = await mergeProviderEnv(options.env ?? process.env);
  const transport = options.transport ?? env.DSH_AIS_TRANSPORT ?? "host";
  let child = options.child;
  let backend = options.backend;
  if (!backend && transport === "sdk") {
    child = child ?? spawn(options.command ?? env.DSH_SDK_COMMAND ?? "dsh", options.args ?? ["--profile", "headless"], {
      stdio: ["pipe", "pipe", "inherit"],
      env,
      cwd: options.cwd ?? process.cwd(),
    });
    const rpc = createJsonRpcClient({
      stdin: child.stdin,
      stdout: child.stdout,
      includeJsonrpc: true,
      stringIds: true,
    });
    backend = createDshSdkBackend(rpc, {
      ...options,
      provider: options.provider ?? env.DSH_PROVIDER,
      model: options.model ?? env.AIS_MODEL ?? env.MODEL,
    });
  } else if (!backend) {
    const port = options.backendPort ?? await allocateLoopbackPort();
    const baseUrl = `http://127.0.0.1:${port}`;
    child = child ?? spawn(
      options.command ?? "dsh",
      options.args ?? ["--profile", "web", "--host", "127.0.0.1", "--port", String(port)],
      {
        stdio: ["ignore", "pipe", "inherit"],
        env,
        cwd: options.cwd ?? process.cwd(),
      },
    );
    let ready = false;
    for (let attempt = 0; attempt < 80; attempt += 1) {
      try {
        const probe = await fetch(`${baseUrl}/api/host.describe`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            type: "client-request",
            rpcId: "ais-probe",
            method: "host.describe",
            payload: {},
          }),
        });
        if (probe.ok) {
          ready = true;
          break;
        }
      } catch {
        // Host is still binding.
      }
      await new Promise((resolve) => setTimeout(resolve, 150));
    }
    if (!ready) {
      throw new Error(`DeepSeek Harness host did not become ready on ${baseUrl}`);
    }
    backend = createDshHostBackend((path, init) => fetch(`${baseUrl}${path}`, init), {
      cwd: options.cwd ?? process.cwd(),
      provider: options.provider ?? env.DSH_PROVIDER,
      model: options.model ?? env.AIS_MODEL ?? env.MODEL,
    });
  }
  await backend.start();
  const instanceId = options.instanceId ?? newInstanceId("dsh");
  return runRegisteredAis({
    instanceId,
    capabilities: backend.capabilities,
    getStatus: () => backend.status(),
    getEntries: () => backend.entries(),
    submitPrompt: ({ text }) => backend.prompt(text),
    abort: backend.abort ? () => backend.abort() : undefined,
    stop: async () => {
      child?.kill?.("SIGTERM");
    },
  });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  await startDshAis();
}
