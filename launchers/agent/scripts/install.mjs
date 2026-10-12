#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, renameSync, rmSync, symlinkSync, unlinkSync } from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";

function run(command, args, options = {}) {
  return (execFileSync(command, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], ...options }) ?? "").trim();
}
function show(name) {
  try { return JSON.parse(run("treer", ["agent", "admin", "profile", "show", name])); }
  catch (error) {
    if (!error.status || !/profile_not_found|not found|no profile/i.test(`${error.stdout || ""} ${error.stderr || ""}`)) throw error;
    return null;
  }
}
function saveProfile(profile, command, cwd, existing) {
  const prefix = ["agent", "admin", "profile"];
  run("treer", existing
    ? [...prefix, "update", profile.name, "--description", profile.description, "--cwd", cwd, "--command", command, ...profile.args.flatMap((arg) => ["--arg", arg])]
    : [...prefix, "create", profile.name, "--description", profile.description, "--cwd", cwd, command, "--", ...profile.args]);
  const saved = show(profile.name);
  if (!saved || saved.command !== command || saved.cwd !== cwd || JSON.stringify(saved.args) !== JSON.stringify(profile.args)) throw new Error(`Profile verification failed: ${profile.name}`);
  console.log(`Verified profile: ${profile.name}`);
}
function restoreProfile({ name, previous }) {
  const prefix = ["agent", "admin", "profile"];
  if (!previous) { run("treer", [...prefix, "delete", name]); return; }
  const args = previous.args ?? [];
  run("treer", [...prefix, "update", previous.profile_id ?? name,
    "--description", previous.description ?? "", "--cwd", previous.cwd,
    "--command", previous.command,
    ...(args.length ? args.flatMap((arg) => ["--arg", arg]) : ["--clear-args"])]);
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
    if (!["--agent", "--harness", "--source", "--ref", "--repository", "--cwd"].includes(key) || !argv[i + 1] || argv[i + 1].startsWith("--")) throw new Error(`Unknown or incomplete option: ${key}`);
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
  const physicalRoot = realpathSync(machineRoot);
  const cwd = options.cwd ?? identity.agent?.cwd ?? ".";
  const inside = relative(physicalRoot, realpathSync(resolve(physicalRoot, cwd)));
  if (isAbsolute(cwd) || inside === ".." || inside.startsWith(`..${sep}`)) throw new Error("Profile cwd must be relative and inside machine.root");
  const root = join(physicalRoot, ".treer/launchers/agent");
  mkdirSync(root, { recursive: true, mode: 0o700 });
  const lock = join(root, ".install-lock");
  try { mkdirSync(lock); } catch { throw new Error(`Another installation owns ${lock}`); }
  let stage, previous, activated = false;
  const changedProfiles = [];
  try {
    stage = mkdtempSync(join(root, ".install-"));
    const checkout = join(stage, "source");
    mkdirSync(checkout);
    let commit;
    if (options.source) {
      const source = realpathSync(options.source);
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
    if (!existsSync(version)) {
      run("npm", ["ci", "--omit=dev", "--no-audit", "--no-fund"], { cwd: candidate, stdio: ["ignore", "inherit", "inherit"] });
      if (!existsSync(join(candidate, "node_modules/.bin/tsx")) || !existsSync(join(candidate, "node_modules/ws/package.json"))) throw new Error("Runtime dependencies missing after npm ci");
      renameSync(checkout, version);
    }
    // Read every original profile before activation or mutation. A later
    // verification/write error must not leave profiles on a missing entry point.
    const originals = new Map(profiles.map((profile) => [profile.name, show(profile.name)]));
    previous = existsSync(join(root, "current")) ? realpathSync(join(root, "current")) : null;
    activate(root, commit);
    activated = true;
    const command = join(root, "current/launchers/agent/scripts/treer-agent.sh");
    for (const profile of profiles) {
      changedProfiles.push({ name: profile.name, previous: originals.get(profile.name) });
      saveProfile(profile, command, cwd, originals.get(profile.name));
    }
    console.log(`Agent launcher installed: ${version}`);
  } catch (error) {
    const rollbackErrors = [];
    for (const profile of changedProfiles.reverse()) {
      try { restoreProfile(profile); }
      catch (rollbackError) { rollbackErrors.push(`${profile.name}: ${rollbackError.message}`); }
    }
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
try { main(); }
catch (error) {
  console.error(error.message);
  if (error.stderr) console.error(String(error.stderr));
  process.exitCode = 1;
}
