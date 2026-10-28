#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readSync, realpathSync, renameSync, rmSync, symlinkSync, unlinkSync, writeFileSync } from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import { profileTransaction } from "./profile-transaction.mjs";
import { installShim } from "./path-shim.mjs";
import { managedProfile, portableCommand } from "./portable-profile.mjs";
import { digest, regular } from "./bundle-contract.mjs";
import { runtimeStatus } from "./native-runtime.mjs";
const { show, saveProfile, restoreProfile } = profileTransaction("treer");

function run(command, args, options = {}) {
  return (execFileSync(command, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], ...options }) ?? "").trim();
}
function activate(root, target) {
  const temporary = join(root, `.current-${process.pid}`);
  try { symlinkSync(target, temporary); renameSync(temporary, join(root, "current")); }
  finally { if (existsSync(temporary)) unlinkSync(temporary); }
}
function main() {
  const options = { agents: [], repository: "https://github.com/EvoEvolver/treer.git" };
  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i++) {
    const key = argv[i];
    if (key === "--help") {
      console.log("Usage: install.sh [--agent codex|claude|gemini|grok|cursor|opencode]... [--source CHECKOUT] [--repository URL] [--ref COMMIT_OR_REF] [--cwd MACHINE_RELATIVE_DIRECTORY]");
      return;
    }
    if (!["--agent", "--harness", "--source", "--ref", "--repository", "--cwd", "--native-manifest", "--native-sha256"].includes(key) || !argv[i + 1] || argv[i + 1].startsWith("--")) throw new Error(`Unknown or incomplete option: ${key}`);
    const value = argv[++i];
    if (key === "--agent" || key === "--harness") options.agents.push(value);
    else options[key.slice(2)] = value;
  }
  const manifest = JSON.parse(readFileSync(new URL("../profiles.json", import.meta.url), "utf8"));
  if (manifest.schema !== "treer.launcher-profiles/v1") throw new Error("Unsupported launcher profile manifest");
  const selected = options.agents.length ? [...new Set(options.agents)] : manifest.profiles.map((p) => p.id);
  for (const id of selected) if (!manifest.profiles.some((p) => p.id === id)) throw new Error(`Unknown provider: ${id}`);
  const identity = JSON.parse(run("treer", ["whoami"]));
  const machineRoot = identity.machine?.root;
  if (!machineRoot || !isAbsolute(machineRoot)) throw new Error("whoami must return an absolute machine.root");
  const physicalRoot = realpathSync.native(machineRoot);
  const cwd = options.cwd ?? identity.agent?.cwd ?? ".";
  const inside = relative(physicalRoot, realpathSync.native(resolve(physicalRoot, cwd)));
  if (isAbsolute(cwd) || isAbsolute(inside) || inside === ".." || inside.startsWith(`..${sep}`)) throw new Error("Profile cwd must be relative and inside machine.root");
  const root = join(physicalRoot, ".treer/launchers/agent");
  mkdirSync(root, { recursive: true, mode: 0o700 });
  const lock = join(root, ".install-lock");
  try { mkdirSync(lock); } catch { throw new Error(`Another installation owns ${lock}`); }
  let stage, previous, shim, activated = false;
  const changedProfiles = [];
  try {
    stage = mkdtempSync(join(root, ".install-"));
    const checkout = join(stage, "source");
    mkdirSync(checkout);
    let commit;
    if (options.source) {
      const source = realpathSync.native(options.source);
      commit = run("git", ["-C", source, "rev-parse", `${options.ref ?? "HEAD"}^{commit}`]);
      const archive = execFileSync("git", ["-C", source, "archive", commit, "launchers/agent"], { maxBuffer: 32 * 1024 * 1024 });
      execFileSync("tar", ["-xf", "-", "-C", checkout], { input: archive });
    } else {
      run("git", ["init", "-q", checkout]);
      run("git", ["-C", checkout, "remote", "add", "origin", options.repository]);
      run("git", ["-C", checkout, "fetch", "--depth=1", "--filter=blob:none", "origin", options.ref ?? "main"]);
      run("git", ["-C", checkout, "sparse-checkout", "init", "--cone"]);
      run("git", ["-C", checkout, "sparse-checkout", "set", "launchers/agent"]);
      run("git", ["-C", checkout, "checkout", "--detach", "FETCH_HEAD"]);
      commit = run("git", ["-C", checkout, "rev-parse", "HEAD"]);
      rmSync(join(checkout, ".git"), { recursive: true });
    }
    if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error("Source must resolve to a full commit SHA");
    const candidate = join(checkout, "launchers/agent");
    const candidateManifest = JSON.parse(readFileSync(join(candidate, "profiles.json"), "utf8"));
    const profiles = selected.map((id) => candidateManifest.profiles.find((p) => p.id === id));
    if (candidateManifest.schema !== manifest.schema || profiles.some((p) => !p)) throw new Error("Selected providers absent from candidate manifest");
    const version = join(root, commit);
    let native;
    if (options["native-manifest"] || options["native-sha256"]) {
      if (!isAbsolute(options["native-manifest"] || "") || !/^[a-f0-9]{64}$/.test(options["native-sha256"] || "")) throw new Error("Explicit native manifest and SHA256 required");
      native = regular(options["native-manifest"], 65536);
      if (digest(native) !== options["native-sha256"]) throw new Error("Native manifest integrity mismatch");
    }
    if (!existsSync(version)) {
      if (native) writeFileSync(join(candidate, "native-runtime.json"), native, { mode: 0o600, flag: "wx" });
      if (runtimeStatus(candidate).default !== "rust" && selected.some(id => ["codex", "claude"].includes(id))) throw new Error("Native default manifest required; no Node fallback");
      run("npm", ["ci", "--omit=dev", "--no-audit", "--no-fund"], { cwd: candidate, stdio: ["ignore", "inherit", "inherit"] });
      renameSync(checkout, version);
    }
    const installed = join(version, "launchers/agent");
    if (native && !regular(join(installed, "native-runtime.json"), 65536).equals(native)) throw new Error("Immutable installed native binding differs");
    if (runtimeStatus(installed).default !== "rust" && selected.some(id => ["codex", "claude"].includes(id))) throw new Error("Native default manifest required; no Node fallback");
    // Read every original profile before activation or mutation. A later
    // verification/write error must not leave profiles on a missing entry point.
    const originals = new Map(profiles.map((profile) => [profile.name, show(profile.name)]));
    for (const profile of profiles) if (!managedProfile(originals.get(profile.name), profile)) throw new Error(`Launcher profile is customized: ${profile.name}`);
    previous = existsSync(join(root, "current")) ? realpathSync.native(join(root, "current")) : null;
    activate(root, commit);
    activated = true;
    shim = installShim(process.execPath, join(root, "current/launchers/agent/scripts/treer-agent.mjs"));
    for (const profile of profiles) {
      changedProfiles.push({ name: profile.name, previous: originals.get(profile.name) });
      saveProfile(profile, portableCommand, ".", originals.get(profile.name));
    }
    console.log(`Agent launcher installed: ${version}`);
  } catch (error) {
    const rollbackErrors = [];
    for (const profile of changedProfiles.reverse()) {
      try { restoreProfile(profile); }
      catch (rollbackError) { rollbackErrors.push(`${profile.name}: ${rollbackError.message}`); }
    }
    try { shim?.rollback(); } catch (rollbackError) { rollbackErrors.push("shim: " + rollbackError.message); }
    if (rollbackErrors.length) {
      // If the control plane is unavailable, keep an executable entry point
      // for any profile that could not be undone, including a first install.
      console.error(`Profile rollback incomplete; keeping usable candidate current: ${rollbackErrors.join("; ")}`);
    } else if (activated) {
      if (previous) activate(root, previous);
      else unlinkSync(join(root, "current"));
    }
    throw error;
  } finally {
    if (stage) rmSync(stage, { recursive: true, force: true });
    rmSync(lock, { recursive: true });
  }
}
try {
  if (process.argv[2] === "--controller-request") {
    const { controllerInstall } = await import("./controller-install.mjs");
    const chunks = [], buffer = Buffer.alloc(4096); let length = 0, read;
    while ((read = readSync(0, buffer, 0, buffer.length, null)) > 0) {
      length += read; if (length > 128 * 1024) throw new Error("Controller install request too large");
      chunks.push(Buffer.from(buffer.subarray(0, read)));
    }
    console.log(JSON.stringify(controllerInstall(JSON.parse(Buffer.concat(chunks).toString("utf8")))));
  } else main();
}
catch (error) {
  const safeMessage = error instanceof SyntaxError ? "Invalid launcher installation metadata" : error.message;
  console.error(safeMessage);
  if (process.argv[2] === "--controller-request") {
    const message = String(safeMessage);
    const code = /rollback incomplete/.test(message) ? "profile_rollback_incomplete" : /Profile verification failed/.test(message) ? "profile_verification_failed" : /owns the lock/.test(message) ? "install_locked" : /lacks launcher|protocol\/contract mismatch|handshake unavailable|security floor/.test(message) ? "launcher_incompatible" : "launcher_install_failed";
    console.log(JSON.stringify({ error: { code, message } }));
  }
  if (process.argv[2] !== "--controller-request" && error.stderr) console.error(String(error.stderr));
  process.exitCode = 1;
}
