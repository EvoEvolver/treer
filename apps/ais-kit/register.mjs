import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

export async function registerTreerInterface({
  port,
  instanceId,
  capabilities,
  uiPath,
  run = execFileAsync,
} = {}) {
  if (!port || !instanceId) throw new Error("AIS registration requires port and instance_id");
  const args = [
    "interface", "register",
    "--port", String(port),
    "--instance-id", instanceId,
  ];
  if (uiPath) args.push("--ui-path", uiPath);
  for (const capability of capabilities ?? []) {
    args.push("--capability", capability);
  }
  await run("treer", args);
}

export async function clearTreerInterface(run = execFileAsync) {
  await run("treer", ["interface", "clear"]).catch(() => {});
}

export function startRegistrationHeartbeat(register, intervalMs = 20000) {
  const timer = setInterval(() => {
    register().catch(() => {});
  }, intervalMs);
  timer.unref?.();
  return () => clearInterval(timer);
}
