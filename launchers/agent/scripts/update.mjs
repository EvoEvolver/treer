// Command frontend only. The maintained bundle installer still owns all
// verification, locking, profiles and pointer publication.
import { existsSync, realpathSync, statSync } from "node:fs";
import { basename, dirname, isAbsolute, join, toNamespacedPath } from "node:path";
import { regular, verifyBundle } from "./bundle-contract.mjs";
import { runtimeStatus } from "./native-runtime.mjs";
import { main as deliver } from "./bundle-launcher.mjs";
import { runProviderSync } from "./process.mjs";
import { latestUpdate } from "./latest-update.mjs";

const usage = "treer-agent update [--controller ABS] [--treer ABS]\n       treer-agent update --latest [--treer ABS] [--root ABS]\n       treer-agent update --bundle ABS --sha256 MANIFEST_SHA256 [--treer ABS] [--root ABS]";
const notes = "--latest downloads the official launcher-stable GitHub release for this platform, verifies its SHA256 closure and requires compatible already-installed native tool bindings. HTTPS/repository custody is the trust source, not an independent signature or rollback guarantee. Same release is unchanged. Optional --release-url accepts the same official repository or HTTP loopback for controlled verification. Without --latest/--bundle, use the local Controller's embedded payload with --require-runtime rust (older Controllers or Node-only payloads refuse without fallback). Alternatively select an operator-approved machine-specific Rust prepared bundle and independently checked bundle-manifest.json SHA256. Node.js 22+ and an enrolled Controller are required. No providers, Agents, Host or Controller are started/restarted.";

function commandJson(command, args, timeout = 20000) {
  const result = runProviderSync(command, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout, maxBuffer: 4 * 1024 * 1024 });
  if (result.error || result.status !== 0) throw new Error(`Launcher update bridge failed (${Number.isInteger(result.status) ? `exit ${result.status}` : "spawn/timeout failure"}). A Controller supporting --require-runtime rust and a verified embedded Rust payload are required; otherwise use --bundle ABS --sha256 HASH. No retry or unguarded fallback was performed; inspect local launcher status before retrying.`);
  try { return JSON.parse(result.stdout); } catch { throw new Error("Launcher update bridge returned invalid JSON; inspect local launcher status before retrying"); }
}

// Trusted local command selectors are not bundle members. Resolve an explicit
// installed symlink and inspect metadata without reading a large executable
// into memory or applying the payload member's 32 MiB limit.
export function updateExecutable(path, label) {
  try {
    if (!isAbsolute(path || "")) throw new Error();
    const resolved = realpathSync.native(path), info = statSync(resolved);
    if (!info.isFile() || info.size < 1 || info.size > 512 * 1024 * 1024) throw new Error();
    return resolved;
  } catch { throw new Error(`Invalid ${label} executable selector: expected an existing regular local file up to 512 MiB`); }
}

export async function update(argv, entryRoot, report = value => console.error(JSON.stringify(value))) {
  if (argv.length === 1 && ["--help", "-h"].includes(argv[0])) return { usage, notes };
  const options = {};
  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i];
    if (Object.hasOwn(options, flag)) throw new Error(usage);
    if (flag === "--latest") { options[flag] = true; continue; }
    if (!["--bundle", "--sha256", "--treer", "--root", "--controller", "--release-url"].includes(flag) || !argv[i + 1] || argv[i + 1].startsWith("--")) throw new Error(usage);
    options[flag] = argv[++i];
  }
  const bundled = options["--bundle"] !== undefined || options["--sha256"] !== undefined;
  if (options["--latest"] && (bundled || options["--controller"]) || options["--release-url"] && !options["--latest"]) throw new Error(usage);
  if (bundled && (options["--controller"] || !isAbsolute(options["--bundle"] || "") || !/^[a-f0-9]{64}$/.test(options["--sha256"] || ""))) throw new Error(usage);
  if (bundled) {
    const candidate = verifyBundle(options["--bundle"], `sha256-${options["--sha256"]}`);
    const runtime = runtimeStatus(options["--bundle"]);
    if (!candidate.portable || runtime.default !== "rust" || ![runtime.codex, runtime.claude].includes("rust")) throw new Error("Update requires a verified runtime-ready Rust bundle with an available Codex or Claude provider; Node/unconfigured candidates are refused before activation");
  }

  // Installed shims pin the CLI in current.json via installed-runner. Bind
  // the default root to that exact installation, not the caller's cwd.
  const base = dirname(entryRoot);
  const installed = /^sha256-[a-f0-9]{64}$/.test(basename(entryRoot)) && basename(base) === "agent" && basename(dirname(base)) === "launchers" && basename(dirname(dirname(base))) === ".treer";
  const installedRoot = installed ? dirname(dirname(dirname(base))) : null;
  if (installed && options["--root"] && options["--root"] !== installedRoot) throw new Error("Update root differs from this installed entry; invoke the intended installation explicitly");
  let treer = options["--treer"] || process.env.AGENT_UI_TREER_BIN;
  if (!treer && installed) treer = JSON.parse(regular(join(base, "current.json"), 65536)).treer;
  if (!isAbsolute(treer || "")) throw new Error("Select trusted absolute --treer (installed entries use their pinned Treer CLI)");
  if (options["--latest"]) {
    treer = updateExecutable(treer, "Treer CLI");
    const controller = commandJson(treer, ["interface", "contract"]);
    if (!isAbsolute(controller.machine?.root || "") || !controller.machine?.server_id) throw new Error("Controller must report an authenticated machine context");
    const root = realpathSync.native(options["--root"] || installedRoot || controller.machine.root);
    if (installedRoot && toNamespacedPath(realpathSync.native(controller.machine.root)) !== toNamespacedPath(installedRoot)) throw new Error("Selected Controller root differs from this installed entry");
    const selected = ["--treer", treer, "--root", root];
    const before = await deliver(["status", ...selected]);
    return latestUpdate({ url: options["--release-url"], root, treer, before, controller, selected, deliver, report });
  }
  if (!bundled) {
    if (options["--root"]) throw new Error("An alternate installation root requires explicit --bundle/--sha256 delivery");
    treer = updateExecutable(treer, "Treer CLI");
    const controllerName = process.platform === "win32" ? "treer-agent-server.exe" : "treer-agent-server";
    const candidates = options["--controller"] ? [options["--controller"]] : [join(dirname(treer), controllerName), join(dirname(dirname(treer)), "libexec", controllerName)];
    const selectedController = candidates.find(path => isAbsolute(path) && existsSync(path));
    if (!selectedController) throw new Error("Local Controller executable not found; select trusted absolute --controller or use --bundle ABS --sha256 HASH");
    const controller = updateExecutable(selectedController, "Controller");
    const context = commandJson(treer, ["interface", "contract"]);
    if (!isAbsolute(context.machine?.root || "") || !context.machine?.server_id) throw new Error("Controller must report an authenticated machine context");
    if (installedRoot && toNamespacedPath(realpathSync.native(context.machine.root)) !== toNamespacedPath(installedRoot)) throw new Error("Selected Controller root differs from this installed entry");
    const prefix = ["launcher", "--node", realpathSync.native(process.execPath), "--treer", realpathSync.native(treer)];
    const before = commandJson(controller, [...prefix, "status"]);
    report({ operation: "update", stage: "controller-preflight", current: before.current, target: "selected Controller embedded Rust payload", running_agents: "unchanged" });
    const result = commandJson(controller, [...prefix, "update", "--require-runtime", "rust"], 400000);
    if (!/^sha256-[a-f0-9]{64}$/.test(result.version || "") || result.runtime?.default !== "rust" || ![result.runtime.codex, result.runtime.claude].includes("rust")) throw new Error("Controller returned an unverified Rust update result; inspect local launcher status before retrying");
    return { ...result, schema: "treer.launcher-update-result/v1", current: before.current, target: result.version, changed: before.current !== result.version, status: "updated", running_agents: "unchanged" };
  }
  const selected = ["--treer", treer, ...(options["--root"] || installedRoot ? ["--root", options["--root"] || installedRoot] : [])];
  const before = await deliver(["status", ...selected]);
  const current = before.current;
  const target = `sha256-${options["--sha256"]}`;
  // The installer verifies authenticated compatibility, file hashes and the
  // Rust requirement again under its existing installation lock.
  report({ operation: "update", stage: "verified-candidate", current, target, runtime: "rust", running_agents: "unchanged" });
  const result = await deliver(["update", ...selected, "--bundle", options["--bundle"], "--sha256", options["--sha256"], "--require-runtime", "rust"]);
  return { ...result, schema: "treer.launcher-update-result/v1", current, target: result.version, changed: current !== result.version, status: "updated", runtime: result.runtime, running_agents: "unchanged", note: "New starts use this default; existing sessions retain their startup version. No lifecycle action was requested." };
}
