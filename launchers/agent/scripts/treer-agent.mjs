#!/usr/bin/env node
// The shim remains cross-platform; Codex/Claude execute the pinned native image.
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { nativeRuntime } from "./native-runtime.mjs";
import { spawnProvider, stopProcessTree } from "./process.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2), env = { ...process.env };
if (args[0] === "update") {
  const { update } = await import("./update.mjs");
  try { console.log(JSON.stringify(await update(args.slice(1), root), null, 2)); }
  catch (error) { console.error(JSON.stringify({ error: { code: "launcher_update_failed", message: error.message } })); process.exitCode = 1; }
} else if (args[0] === "session") {
  // Offline operations need no provider; explicit launch requires the pinned
  // Claude binding and uses the same native image. Never fall back to Node.
  const running = ["launch", "run"].includes(args[1]);
  const binding = nativeRuntime(root, running ? ["claude"] : []);
  // Also fail closed if an installer pairs this dispatcher with an older
  // image that ignores argv: a regular executable is never a runtime cwd,
  // and this harness name cannot select Codex/Claude. The new offline CLI
  // takes its cwd/session exclusively from explicit arguments.
  const offlineEnv = { ...env, AGENT_UI_CWD: binding.executable.path, AGENT_UI_HARNESS: "offline-session", AGENT_UI_PUBLIC_DIR: binding.public_dir };
  if (running) {
    offlineEnv.RUST_SPIKE_REGISTER = "1";
    // A saved Host startup must never inherit a parent's observation file.
    if (args[1] === "run") delete offlineEnv.AGENT_UI_PORT_FILE;
  }
  const child = spawnProvider(binding.executable.path, args, { env: offlineEnv, stdio: "inherit" });
  child.on("error", () => { process.exitCode = 1; });
  child.on("exit", (code, signal) => { process.exitCode = code ?? (signal ? 1 : 0); });
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) process.on(signal, () => stopProcessTree(child));
} else if (args.length === 1 && ["--help", "-h", "help"].includes(args[0])) {
  console.log("       treer-agent session --help  (Claude export/import/prepare/launch/persistent run; macOS/Linux)");
  console.log("Usage: treer-agent [--harness codex|claude] [--session-id ID|--thread-id ID] [--cwd ABS] [--name NAME]\n       treer-agent update [--controller ABS] [--treer ABS]\n       treer-agent update --latest [--treer ABS] [--root ABS]\n       treer-agent update --bundle ABS --sha256 MANIFEST_SHA256 [--treer ABS] [--root ABS]\n       treer-agent update --help\nUpdate changes only the default for future starts; running Agents/Host/Controller are not restarted.");
} else {
const mapping = { "--session-id": "AGENT_UI_SESSION_ID", "--thread-id": "AGENT_UI_SESSION_ID", "--cwd": "AGENT_UI_CWD", "--name": "AGENT_UI_NAME" };
let harness = env.AGENT_UI_HARNESS || "codex";
for (let i = 0; i < args.length; i += 2) {
  const [key, value] = args.slice(i, i + 2);
  if (typeof value !== "string" || !value) throw new Error("Incomplete launcher argument");
  // Saved profiles may still carry `--runtime rust`; it is the only runtime.
  if (key === "--runtime") { if (value !== "rust") throw new Error("The Node runtime was removed; only --runtime rust is supported"); continue; }
  if (key === "--harness") harness = value;
  else if (mapping[key]) env[mapping[key]] = value;
  else throw new Error("Unsupported native launcher argument");
}
if (!["codex", "claude"].includes(harness)) throw new Error("Unsupported harness; select codex or claude");
const binding = nativeRuntime(root, [harness]);
env.AGENT_UI_HARNESS = harness; env.AGENT_UI_CWD ||= env.CODEX_UI_CWD || process.cwd();
// A parent's port observation file belongs to that invocation. Rust owns
// its own registration, so no outer-wrapper port file is required here.
delete env.AGENT_UI_PORT_FILE;
env.AGENT_UI_PUBLIC_DIR = binding.public_dir; env.RUST_SPIKE_REGISTER = "1";
const child = spawnProvider(binding.executable.path, [], { env, stdio: "inherit" });
child.on("error", () => { process.exitCode = 1; });
child.on("exit", (code, signal) => { process.exitCode = code ?? (signal ? 1 : 0); });
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) process.on(signal, () => stopProcessTree(child));
}
