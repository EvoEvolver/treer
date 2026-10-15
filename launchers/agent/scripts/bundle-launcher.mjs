#!/usr/bin/env node
// Local bundle delivery is an explicit operator action, not an official release.
// The same installer owns profile receipts, immutable assets and current.json.
import { randomUUID } from "node:crypto";
import { closeSync, existsSync, fsyncSync, lstatSync, mkdirSync, openSync, realpathSync, renameSync, rmSync, rmdirSync, writeFileSync } from "node:fs";
import { get } from "node:http";
import { dirname, isAbsolute, join, parse, relative, resolve, sep, toNamespacedPath } from "node:path";
import { fileURLToPath } from "node:url";
import { compatibility, digest, regular, verifyBundle } from "./bundle-contract.mjs";
import { controllerInstall } from "./controller-install.mjs";
import { profileFailureSummary } from "./profile-transaction.mjs";
import { runProviderSync } from "./process.mjs";
import { protectDirectory, syncDirectory } from "./private-state.mjs";

const usage = "bundle-launcher.mjs install|update|status|rollback|rollout --treer ABS [--root ABS] [--bundle ABS --sha256 MANIFEST_SHA256] [--skip-profiles] [--agent PROVIDER] [--targets JSON_FILE --preview TARGET --rollout-id ID --batch-size 3 --timeout 90000 --apply]";
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
export function parseOptions(argv) {
  const options = { operation: argv[0], agents: [], batchSize: 3, timeout: 90000, apply: false, skipProfiles: false };
  if (!argv.length || argv[0] === "--help") return { help: usage };
  if (!["install", "update", "status", "rollback", "rollout"].includes(options.operation)) throw new Error(usage);
  for (let i = 1; i < argv.length; i++) {
    const flag = argv[i];
    if (flag === "--apply") options.apply = true;
    else if (flag === "--skip-profiles") options.skipProfiles = true;
    else {
      if (!["--treer", "--root", "--bundle", "--sha256", "--agent", "--cwd", "--version", "--targets", "--preview", "--rollout-id", "--batch-size", "--timeout"].includes(flag) || !argv[i + 1]) throw new Error(usage);
      const value = argv[++i];
      if (flag === "--agent") options.agents.push(value);
      else options[flag.slice(2).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase())] = value;
    }
  }
  options.batchSize = Number(options.batchSize); options.timeout = Number(options.timeout);
  if (!Number.isInteger(options.batchSize) || options.batchSize < 1 || options.batchSize > 3) throw new Error("Batch size must be 1–3; the first batch is always the single preview Agent");
  if (!Number.isInteger(options.timeout) || options.timeout < 1000 || options.timeout > 300000) throw new Error("Health timeout must be 1000–300000 milliseconds");
  if (!isAbsolute(options.treer || "")) throw new Error("Select an explicit trusted absolute --treer executable");
  regular(options.treer); options.treer = realpathSync.native(options.treer);
  if (Number(process.versions.node.split(".")[0]) < 22) throw new Error("Node.js 22+ is required");
  return options;
}

function cli(options, args, json = true, input) {
  const result = runProviderSync(options.treer, args, { encoding: "utf8", stdio: [input ? "pipe" : "ignore", "pipe", "pipe"], input, timeout: 20000, maxBuffer: 2 * 1024 * 1024 });
  if (result.error || result.status !== 0) throw new Error(profileFailureSummary(result));
  if (!json) return;
  try { return JSON.parse(result.stdout); } catch { throw new Error("Treer returned an invalid JSON response"); }
}
function physicalDirectory(path, create = false) {
  if (!isAbsolute(path || "")) throw new Error("An absolute private directory is required");
  let current = parse(resolve(path)).root;
  for (const part of relative(current, resolve(path)).split(sep).filter(Boolean)) {
    current = join(current, part);
    if (!existsSync(current) && create) mkdirSync(current, { mode: 0o700 });
    const info = lstatSync(current);
    if (!info.isDirectory() || info.isSymbolicLink()) throw new Error("Private directory ancestors must not be links or special files");
  }
  const physical = realpathSync.native(path);
  if (toNamespacedPath(physical) !== toNamespacedPath(resolve(path))) throw new Error("Private directory must use its physical path");
  return physical;
}
function atomic(path, value) {
  if (existsSync(path)) regular(path, 2 * 1024 * 1024);
  const bytes = JSON.stringify(value) + "\n";
  if (Buffer.byteLength(bytes) > 2 * 1024 * 1024) throw new Error("Rollout journal exceeds its 2 MiB limit");
  const temporary = join(dirname(path), `.rollout-${randomUUID()}.tmp`), handle = openSync(temporary, "wx", 0o600);
  try {
    try { writeFileSync(handle, bytes); fsyncSync(handle); } finally { closeSync(handle); }
    renameSync(temporary, path); syncDirectory(dirname(path));
  } finally { if (existsSync(temporary)) rmSync(temporary); }
}
function context(options) {
  const controller = cli(options, ["interface", "contract"]);
  if (!controller.machine?.server_id || !isAbsolute(controller.machine.root || "")) throw new Error("Running Controller must report an authenticated machine context");
  const root = physicalDirectory(options.root ?? controller.machine.root);
  const base = join(root, ".treer", "launchers", "agent");
  physicalDirectory(base, true); protectDirectory(base);
  return { controller, root, base };
}
function bundle(options, controller) {
  if (!isAbsolute(options.bundle || "") || !/^[a-f0-9]{64}$/.test(options.sha256 || "")) throw new Error("A prepared --bundle directory and independently checked --sha256 manifest hash are required");
  const version = `sha256-${options.sha256}`;
  const verified = verifyBundle(physicalDirectory(options.bundle), version);
  if (!verified.portable) throw new Error("Only offline runtime-ready prepared payloads are accepted; no npm/network installation is performed");
  const aligned = compatibility(verified.contract, controller);
  if (aligned.status !== "compatible") throw new Error(aligned.reason);
  return { version, source: verified.manifest.source_commit, contract: verified.contract };
}
function install(options, ctx, selected, operation = options.operation, version) {
  return controllerInstall({ schema: "treer.launcher-install/v1", operation, root: ctx.root, cwd: options.cwd ?? ".", agents: options.agents,
    skip_profiles: options.skipProfiles, node: realpathSync.native(process.execPath), treer: options.treer, controller: ctx.controller,
    bundle_dir: options.bundle, version: selected?.version, source_commit: selected?.source, allow_network: false, rollback_version: version });
}
function localGet(port, path) {
  if (!Number.isInteger(port) || port < 1 || port > 65535) return Promise.reject(new Error("Agent has no valid local interface port"));
  return new Promise((done, fail) => {
    const request = get({ hostname: "127.0.0.1", port, path }, (response) => {
      let bytes = 0; const chunks = [];
      response.on("data", (chunk) => { bytes += chunk.length; if (bytes > 2 * 1024 * 1024) request.destroy(new Error("Agent response exceeds limit")); else chunks.push(chunk); });
      response.on("error", fail); response.on("end", () => {
        if (response.statusCode !== 200) { fail(new Error("Agent UI/interface did not return HTTP 200")); return; }
        try { done(path === "/" ? true : JSON.parse(Buffer.concat(chunks).toString("utf8"))); } catch { fail(new Error("Invalid Agent interface response")); }
      });
    });
    request.setTimeout(2000, () => request.destroy(new Error("Agent interface request timed out"))); request.on("error", fail);
  });
}
async function inspect(options, target, ctx) {
  const agent = cli(options, ["agent", "show", target]);
  if (!/^ag_[A-Za-z0-9_-]+$/.test(agent.agent_id || "") || agent.server_id !== ctx.controller.machine.server_id || agent.kind !== "command") throw new Error("Rollout targets must be command Agents on this Controller's machine");
  if (agent.name?.toLowerCase() === "leader" || /(^|\/)leader$/i.test(agent.path || "") || agent.agent_id === process.env.TREER_AGENT_ID) throw new Error("The leader and the rollout's own Agent are excluded; switch them manually");
  const state = await localGet(agent.interface?.port, "/api/state");
  if (!state.ready || !state.thread?.id) throw new Error("Target must have a healthy launcher and native session before rollout");
  const cwd = physicalDirectory(state.thread.cwd), inside = relative(toNamespacedPath(physicalDirectory(ctx.controller.machine.root)), toNamespacedPath(cwd));
  if (isAbsolute(inside) || inside === ".." || inside.startsWith(`..${sep}`)) throw new Error("Target native cwd must remain inside the enrolled machine root");
  return { agent, state };
}
function replacement(startup, snapshot, ctx) {
  let args = startup.args ?? [];
  const entry = /(?:^|[\\/])(?:run\.mjs|treer-agent\.mjs)$/i.test(args[0] || "");
  if (entry) args = args.slice(1);
  else if (!/(?:^|[\\/])treer-agent\.sh$/i.test(startup.command || "")) throw new Error("Only existing launcher startup commands can be migrated automatically");
  const allowed = new Set(["--harness", "--session-id", "--thread-id", "--cwd", "--acp-command", "--acp-arg", "--name"]), preserved = [];
  for (let i = 0; i < args.length; i += 2) {
    if (!allowed.has(args[i]) || typeof args[i + 1] !== "string") throw new Error("Unknown saved launcher argument; preserve custom startup manually");
    if (!["--session-id", "--thread-id", "--cwd"].includes(args[i])) preserved.push(args[i], args[i + 1]);
  }
  if (!preserved.includes("--harness")) preserved.push("--harness", snapshot.harness.id);
  if (preserved[preserved.indexOf("--harness") + 1] !== snapshot.harness.id) throw new Error("Saved provider differs from the running harness; refusing to overwrite it");
  return { command: realpathSync.native(process.execPath), cwd: startup.cwd,
    args: [join(ctx.base, "run.mjs"), ...preserved, "--session-id", snapshot.thread.id, "--cwd", snapshot.thread.cwd] };
}
const specification = (startup) => JSON.stringify({ command: startup.command, cwd: startup.cwd, args: startup.args ?? [] });
function saveStartup(options, target, startup) {
  cli(options, ["agent", "admin", "startup", "set", target, "--cwd", startup.cwd, "--", startup.command, ...startup.args]);
  const readback = cli(options, ["agent", "admin", "startup", "show", target]);
  if (specification(readback) !== specification(startup)) throw new Error("Saved startup readback does not match the rollout plan");
}
async function healthy(options, ctx, item, contract, timeout, requireNew = true) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    try {
      const { agent, state } = await inspect(options, item.id, ctx), iface = agent.interface;
      if (state.thread.id !== item.session) throw new Error("Native session changed during rollout");
      const manifest = await localGet(iface.port, "/v1/manifest"); await localGet(iface.port, "/");
      if ((!requireNew || iface.instance_id !== item.oldInstance) && manifest.instance_id === iface.instance_id && manifest.ui_path === "/" && manifest.protocol === contract.protocol && JSON.stringify([...manifest.capabilities].sort()) === JSON.stringify([...contract.capabilities].sort())) return;
    } catch { /* Only fixed diagnostics are emitted; no response bodies/credentials. */ }
    await sleep(200);
  }
  throw new Error("Health check timed out: require fresh registered instance, UI HTTP 200 and unchanged native session");
}

async function rollout(options, ctx, selected) {
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(options.rolloutId || "") || !isAbsolute(options.targets || "")) throw new Error("rollout requires --rollout-id (1–64 ASCII letters/digits/_/-), --targets ABS_JSON_FILE and --preview TARGET");
  const targets = JSON.parse(regular(options.targets, 65536));
  if (!Array.isArray(targets) || !targets.length || targets.length > 64 || targets.some((target) => typeof target !== "string" || !/^(ag_[A-Za-z0-9_-]+|\/[A-Za-z0-9_./-]+)$/.test(target)) || !targets.includes(options.preview)) throw new Error("Use an explicit target list (at most 64) containing the preview Agent");
  const directory = physicalDirectory(join(ctx.base, "rollouts"), true); protectDirectory(directory);
  const file = join(directory, options.rolloutId + ".json"), lock = join(directory, ".lock");
  try { mkdirSync(lock, { mode: 0o700 }); } catch { throw new Error("A rollout owns the lock; inspect its journal before manual crash recovery"); }
  try {
    const fingerprint = digest(JSON.stringify({ version: selected.version, targets, preview: options.preview, root: ctx.root, agents: options.agents, skipProfiles: options.skipProfiles, cwd: options.cwd ?? ".", treer: options.treer, node: process.execPath, batchSize: options.batchSize, timeout: options.timeout }));
    let journal;
    if (existsSync(file)) {
      journal = JSON.parse(regular(file, 2 * 1024 * 1024));
      if (journal.schema !== "treer.launcher-rollout/v1" || journal.fingerprint !== fingerprint) throw new Error("Rollout ID was already used for a different plan");
      if (!["planned", "switching", "completed"].includes(journal.status)) throw new Error("Previous rollout failed or rollback was interrupted; inspect its recorded reason and recover before using a new rollout ID");
    } else {
      const previous = install(options, ctx, undefined, "status").current;
      if (!previous) throw new Error("Install a baseline launcher before rollout so failure has a rollback target");
      const items = [], ids = new Set();
      for (const target of [options.preview, ...targets.filter((target) => target !== options.preview)]) {
        const { agent, state } = await inspect(options, target, ctx);
        if (ids.has(agent.agent_id)) throw new Error("Target aliases refer to the same Agent"); ids.add(agent.agent_id);
        const original = cli(options, ["agent", "admin", "startup", "show", agent.agent_id]);
        if (original.agent_id !== agent.agent_id || original.enabled === false) throw new Error("Only enabled target startups can be migrated");
        items.push({ id: agent.agent_id, path: agent.path, session: state.thread.id, oldInstance: agent.interface.instance_id, oldGeneration: agent.run_generation, original, replacement: replacement(original, state, ctx), phase: "pending" });
      }
      journal = { schema: "treer.launcher-rollout/v1", fingerprint, id: options.rolloutId, version: selected.version, source: selected.source, previous, status: "planned", items };
    }
    const summary = () => ({ rollout_id: journal.id, version: journal.version, source: journal.source, status: journal.status, journal: file, agents: journal.items.map((item) => ({ id: item.id, path: item.path, phase: item.phase })) });
    if (!options.apply) return { ...summary(), dry_run: true, first_batch: [journal.items[0].id], remaining_batch_size: options.batchSize };
    atomic(file, journal);
    if (journal.status === "completed") {
      if (install(options, ctx, undefined, "status").current !== selected.version) throw new Error("Completed rollout is no longer the active version; no action taken");
      for (const item of journal.items) await healthy(options, ctx, item, selected.contract, options.timeout);
      return { ...summary(), replay: true };
    }
    try {
      journal.stage = "install"; atomic(file, journal);
      install(options, ctx, selected, "update"); journal.status = "switching"; atomic(file, journal);
      // Preview is always alone. Each subsequent batch is bounded; there is no
      // automatic workspace-wide selection and the leader is never selected.
      const batches = [[journal.items[0]]];
      for (let i = 1; i < journal.items.length; i += options.batchSize) batches.push(journal.items.slice(i, i + options.batchSize));
      for (const batch of batches) {
        if (install(options, ctx, undefined, "status").current !== selected.version) throw new Error("Launcher pointer changed outside this rollout");
        for (const item of batch) {
          if (item.phase === "continued") continue;
          const saved = cli(options, ["agent", "admin", "startup", "show", item.id]);
          if (saved.enabled === false) throw new Error("Target was stopped outside this rollout; refusing to restart it");
          if (![specification(item.original), specification(item.replacement)].includes(specification(saved))) throw new Error("Target startup changed outside this rollout; refusing to overwrite it");
          journal.stage = `startup:${item.id}`;
          if (item.phase === "pending") { item.phase = "changing"; atomic(file, journal); }
          if (specification(saved) !== specification(item.replacement)) saveStartup(options, item.id, item.replacement);
          const current = cli(options, ["agent", "show", item.id]);
          item.phase = "restarting"; journal.stage = `restart:${item.id}`; atomic(file, journal);
          // A lost restart receipt must not blindly restart the same Agent again.
          if (current.run_generation === item.oldGeneration && current.interface?.instance_id === item.oldInstance) cli(options, ["agent", "admin", "restart", item.id]);
          item.phase = "checking"; atomic(file, journal);
        }
        for (const item of batch) {
          if (item.phase === "continued") continue;
          journal.stage = `health:${item.id}`; atomic(file, journal);
          await healthy(options, ctx, item, selected.contract, options.timeout);
          item.phase = "continuing"; journal.stage = `continue:${item.id}`; atomic(file, journal);
          cli(options, ["message", "send", "--to", item.id, "--idempotency-key", `launcher-rollout/${journal.id}/${item.id}/continue`], true, "Launcher update completed and your native session was preserved. Continue your previous task.\n");
          item.phase = "continued"; atomic(file, journal);
        }
      }
      journal.status = "completed"; atomic(file, journal); return summary();
    } catch (error) {
      journal.status = "rolling-back"; journal.reason = `Rollout failed at ${journal.stage}; previous version/startups are being restored`; atomic(file, journal);
      try {
        const current = install(options, ctx, undefined, "status").current;
        if (current !== journal.previous && current !== selected.version) throw new Error("Launcher pointer changed outside this rollout");
        if (current !== journal.previous) install(options, ctx, undefined, "rollback", journal.previous);
        const previousContract = verifyBundle(join(ctx.base, journal.previous), journal.previous, undefined, true).contract;
        for (const item of journal.items) if (item.phase !== "pending") {
          const saved = cli(options, ["agent", "admin", "startup", "show", item.id]);
          if (specification(saved) === specification(item.replacement)) saveStartup(options, item.id, item.original);
          else if (specification(saved) !== specification(item.original)) throw new Error("Target startup changed outside this rollout during rollback");
          const agent = cli(options, ["agent", "show", item.id]);
          if (saved.enabled !== false && (agent.run_generation !== item.oldGeneration || agent.interface?.instance_id !== item.oldInstance)) cli(options, ["agent", "admin", "restart", item.id]);
          if (saved.enabled !== false) await healthy(options, ctx, item, previousContract, options.timeout, false);
          item.phase = saved.enabled === false ? "restored-stopped" : "restored"; atomic(file, journal);
        }
        journal.status = "rolled-back";
      } catch { journal.status = "rollback-failed"; journal.reason += "; rollback incomplete, inspect private journal and target state before proceeding"; }
      atomic(file, journal);
      throw new Error(`${journal.status}: ${journal.reason}`);
    }
  } finally { rmdirSync(lock); }
}

export async function main(argv) {
  const options = parseOptions(argv);
  if (options.help) return { usage: options.help };
  const ctx = context(options);
  if (options.operation === "status" || options.operation === "rollback") return install(options, ctx, undefined, options.operation, options.version);
  const selected = bundle(options, ctx.controller);
  if (options.operation === "rollout") return rollout(options, ctx, selected);
  return { ...install(options, ctx, selected), delivery: "local-prepared-bundle", manifest_sha256: options.sha256 };
}
if (process.argv[1] && realpathSync.native(process.argv[1]) === realpathSync.native(fileURLToPath(import.meta.url))) {
  try { console.log(JSON.stringify(await main(process.argv.slice(2)), null, 2)); }
  catch (error) { console.error(JSON.stringify({ error: { code: "launcher_bundle_delivery_failed", message: error.message } })); process.exitCode = 1; }
}
