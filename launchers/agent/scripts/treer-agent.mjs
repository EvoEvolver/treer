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
  const binding = nativeRuntime(root, args[1] === "launch" ? ["claude"] : []);
  // Also fail closed if an installer pairs this dispatcher with an older
  // image that ignores argv: a regular executable is never a runtime cwd,
  // and this harness name cannot select Codex/Claude. The new offline CLI
  // takes its cwd/session exclusively from explicit arguments.
  const offlineEnv = { ...env, AGENT_UI_CWD: binding.executable.path, AGENT_UI_HARNESS: "offline-session", AGENT_UI_PUBLIC_DIR: binding.public_dir };
  if (args[1] === "launch") offlineEnv.RUST_SPIKE_REGISTER = "1";
  const child = spawnProvider(binding.executable.path, args, { env: offlineEnv, stdio: "inherit" });
  child.on("error", () => { process.exitCode = 1; });
  child.on("exit", (code, signal) => { process.exitCode = code ?? (signal ? 1 : 0); });
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) process.on(signal, () => stopProcessTree(child));
} else if (args.length === 1 && ["--help", "-h", "help"].includes(args[0])) {
  console.log("       treer-agent session --help  (Claude export/import/prepare/explicit launch; macOS/Linux)");
  console.log("Usage: treer-agent [--harness codex|claude|acp] [--session-id ID|--thread-id ID] [--cwd ABS] [--name NAME] [--runtime rust|node]\n       treer-agent update [--controller ABS] [--treer ABS]\n       treer-agent update --bundle ABS --sha256 MANIFEST_SHA256 [--treer ABS] [--root ABS]\n       treer-agent update --help\nCodex/Claude default to Rust; ACP explicitly uses Node. Update changes only the default for future starts; running Agents/Host/Controller are not restarted.");
} else {
const mapping = { "--session-id": "AGENT_UI_SESSION_ID", "--thread-id": "AGENT_UI_SESSION_ID", "--cwd": "AGENT_UI_CWD", "--name": "AGENT_UI_NAME" };
let runtime, harness = env.AGENT_UI_HARNESS || "codex", nodeArgs = [];
for (let i = 0; i < args.length; i += 2) {
  const [key, value] = args.slice(i, i + 2);
  if (typeof value !== "string" || !value && key !== "--acp-arg") throw new Error("Incomplete launcher argument");
  if (key === "--runtime") runtime = value;
  else { nodeArgs.push(key, value); if (key === "--harness") harness = value; }
}
// Selecting the ACP harness is an explicit selection of its Node adapter,
// not fallback from a failed native attempt. Existing workspace-global ACP
// profile argv remain usable on machines with older installed entries.
runtime ||= harness === "acp" ? "node" : "rust";
if (!["node", "rust"].includes(runtime)) throw new Error("Select --runtime rust or explicit --runtime node");
if (runtime === "node") {
  process.argv = [process.argv[0], process.argv[1], ...nodeArgs];
  await import("./treer-agent-node.mjs");
} else {
  if (!["codex", "claude"].includes(harness)) throw new Error("ACP is not Rust-adapted; select its explicit --runtime node profile");
  for (let i = 0; i < nodeArgs.length; i += 2) {
    const [key, value] = nodeArgs.slice(i, i + 2);
    if (key !== "--harness" && !mapping[key]) throw new Error("Unsupported native launcher argument");
    if (mapping[key]) env[mapping[key]] = value;
  }
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
}
