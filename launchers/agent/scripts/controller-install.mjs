import { chmodSync, closeSync, fsyncSync, lstatSync, mkdirSync, mkdtempSync, openSync, readdirSync, realpathSync, renameSync, rmSync, rmdirSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { isAbsolute, join, relative, resolve, sep, toNamespacedPath } from "node:path";
import { compatibility, digest, regular, verifyBundle } from "./bundle-contract.mjs";
import { profileTransaction } from "./profile-transaction.mjs";

function present(path) { try { lstatSync(path); return true; } catch (error) { if (error.code === "ENOENT") return false; throw error; } }
function directory(path) {
  if (!present(path)) mkdirSync(path, { mode: 0o700 });
  const info = lstatSync(path);
  if (!info.isDirectory() || info.isSymbolicLink() || process.getuid && info.uid !== process.getuid()) throw new Error("Launcher installation directory is not privately owned or is a link");
  if (process.platform !== "win32") chmodSync(path, 0o700);
}
function atomic(path, bytes) {
  if (present(path)) regular(path);
  const temp = path + `.tmp-${process.pid}`;
  const fd = openSync(temp, "wx", 0o600);
  try { writeFileSync(fd, bytes); fsyncSync(fd); } finally { closeSync(fd); }
  try { renameSync(temp, path); } finally { if (present(temp)) rmSync(temp); }
  if (process.platform !== "win32") { const parent = openSync(resolve(path, ".."), "r"); try { fsyncSync(parent); } finally { closeSync(parent); } }
}
function json(path) { return JSON.parse(regular(path, 2 * 1024 * 1024).toString("utf8")); }
function privateCopy(source, target) {
  mkdirSync(target, { mode: 0o700 });
  for (const name of readdirSync(source)) {
    const from = join(source, name), to = join(target, name), info = lstatSync(from);
    if (info.isSymbolicLink() || !info.isDirectory() && !info.isFile()) throw new Error("Cannot copy linked/special launcher assets");
    if (info.isDirectory()) privateCopy(from, to);
    else { writeFileSync(to, regular(from), { mode: info.mode & 0o111 ? 0o700 : 0o600, flag: "wx" }); }
  }
}
export function controllerInstall(request) {
  if (request.schema !== "treer.launcher-install/v1" || !["install", "update", "status", "rollback"].includes(request.operation)) throw new Error("Unsupported Controller install request");
  if (!isAbsolute(request.root || "") || !isAbsolute(request.node || "") || !isAbsolute(request.treer || "") || !Array.isArray(request.agents || [])) throw new Error("Explicit absolute root/Node/Treer paths are required");
  // Rust canonical paths on Windows use the verbatim prefix. Node's JS
  // realpath walker rejects those (EISDIR at C:); the native API handles them.
  if (Number(process.versions.node.split(".")[0]) < 22 || realpathSync.native(request.node) !== realpathSync.native(process.execPath)) throw new Error("Installer requires the selected Node.js 22+ executable");
  const physicalRoot = realpathSync.native(request.root);
  if (toNamespacedPath(physicalRoot) !== toNamespacedPath(resolve(request.root))) throw new Error("Machine root must be a physical path");
  const cwd = request.cwd ?? ".", cwdInside = relative(physicalRoot, realpathSync.native(resolve(physicalRoot, cwd)));
  if (isAbsolute(cwd) || cwdInside === ".." || cwdInside.startsWith(`..${sep}`)) throw new Error("Profile cwd must stay inside machine root");
  let root = physicalRoot;
  for (const name of [".treer", "launchers", "agent"]) { root = join(root, name); directory(root); }
  const pointerPath = join(root, "current.json"), lock = join(root, ".install-lock");
  try { mkdirSync(lock, { mode: 0o700 }); } catch { throw new Error("Another launcher installation owns the lock; inspect it before manual recovery"); }
  let stage;
  try {
    const pointer = present(pointerPath) ? json(pointerPath) : null;
    const versionName = (value) => { if (!/^sha256-[a-f0-9]{64}$/.test(value || "")) throw new Error("Invalid launcher content version"); return value; };
    const inspect = (version) => {
      const verified = verifyBundle(join(root, versionName(version)), version, undefined, true);
      return { version, source_commit: verified.manifest.source_commit, compatibility: compatibility(verified.contract, request.controller), portable: verified.portable };
    };
    if (request.operation === "status") {
      const versions = readdirSync(root).filter((name) => /^sha256-[a-f0-9]{64}$/.test(name)).map((version) => {
        try { return inspect(version); } catch { return { version, compatibility: { status: "incompatible", reason: "Installed launcher integrity check failed" } }; }
      });
      let entryIntegrity = "missing";
      if (pointer?.version) {
        try { entryIntegrity = digest(regular(join(root, "run.mjs"))) === digest(regular(join(root, versionName(pointer.version), "scripts/installed-runner.mjs"))) ? "valid" : "mismatch"; }
        catch { entryIntegrity = "invalid"; }
      }
      return { schema: "treer.launcher-install-result/v1", operation: "status", current: pointer?.version ?? null, previous: pointer?.previous ?? null, versions, entry_integrity: entryIntegrity,
        legacy_current: !pointer && present(join(root, "current")) ? "legacy pointer present; not migrated" : null };
    }
    const version = versionName(request.operation === "rollback" ? request.rollback_version || pointer?.previous : request.version);
    let versionRoot = join(root, version), verified;
    if (request.operation === "rollback") verified = verifyBundle(versionRoot, version, undefined, true);
    else {
      if (!isAbsolute(request.bundle_dir || "")) throw new Error("An absolute verified bundle directory is required");
      verified = verifyBundle(request.bundle_dir, version, request.source_commit);
      const aligned = compatibility(verified.contract, request.controller);
      if (aligned.status !== "compatible") throw new Error(aligned.reason);
      if (!verified.portable && !request.allow_network) throw new Error("Development payload is not runtime-ready; rebuild official payload or explicitly allow network installation");
      if (!present(versionRoot)) {
        stage = mkdtempSync(join(root, ".install-"));
        const candidate = join(stage, "bundle"); privateCopy(request.bundle_dir, candidate);
        verifyBundle(candidate, version, request.source_commit);
        if (!verified.portable) {
          if (!isAbsolute(request.npm_cli || "")) throw new Error("Explicit npm CLI path required for development network install");
          regular(request.npm_cli);
          try { execFileSync(request.node, [request.npm_cli, "ci", "--omit=dev", "--ignore-scripts", "--no-audit", "--no-fund"], { cwd: candidate, stdio: "ignore", timeout: 300000 }); }
          catch { throw new Error("Development dependency installation failed; candidate was not activated"); }
          regular(join(candidate, "node_modules/tsx/dist/loader.mjs")); regular(join(candidate, "node_modules/ws/package.json"));
        }
        renameSync(candidate, versionRoot);
      }
      // Reuse only verified immutable directories, never overwrite running assets.
      verifyBundle(versionRoot, version, request.source_commit, !verified.portable);
    }
    const aligned = compatibility(verified.contract, request.controller);
    if (aligned.status !== "compatible") throw new Error(aligned.reason);
    const profilesManifest = json(join(versionRoot, "profiles.json"));
    if (profilesManifest.schema !== "treer.launcher-profiles/v1" || !Array.isArray(profilesManifest.profiles)) throw new Error("Invalid profile manifest");
    const selected = request.agents?.length ? [...new Set(request.agents)] : profilesManifest.profiles.map((profile) => profile.id);
    const profiles = selected.map((id) => profilesManifest.profiles.find((profile) => profile.id === id));
    if (profiles.some((profile) => !profile || typeof profile.name !== "string" || !Array.isArray(profile.args))) throw new Error("Unknown launcher provider");
    const transaction = profileTransaction(request.treer, (message) => console.error(message));
    const originals = new Map(profiles.map((profile) => [profile.name, transaction.show(profile.name)]));
    const receiptPath = join(root, "profiles-receipt.json");
    const originalReceipt = present(receiptPath) ? regular(receiptPath) : null;
    const receipt = originalReceipt ? JSON.parse(originalReceipt.toString("utf8")) : {};
    const runner = join(root, "run.mjs");
    const specification = (profile) => ({ command: profile.command, cwd: profile.cwd, args: profile.args ?? [] });
    for (const profile of profiles) {
      const current = originals.get(profile.name);
      if (!current) continue;
      const previous = receipt[profile.name];
      // A first takeover only adopts a known legacy launcher entry, never an
      // arbitrary owner's command. Any subsequent customization is protected.
      const legacy = !previous && current.command === join(root, "current/launchers/agent/scripts/treer-agent.sh") && current.cwd === cwd && JSON.stringify(current.args) === JSON.stringify(profile.args);
      if (!legacy && (!previous || JSON.stringify(specification(current)) !== JSON.stringify(previous))) throw new Error(`Launcher profile is customized; preserve or remove it explicitly before installation: ${profile.name}`);
    }
    const changed = [];
    // A running Node process already loaded this trampoline. Atomically repair
    // it against verified payload bytes without altering versioned runtime assets.
    const runnerBytes = regular(join(versionRoot, "scripts/installed-runner.mjs"));
    if (!present(runner) || digest(regular(runner)) !== digest(runnerBytes)) atomic(runner, runnerBytes);
    const next = { schema: "treer.launcher-current/v1", version, previous: pointer?.version !== version ? pointer?.version ?? null : pointer?.previous ?? null, node: request.node, treer: request.treer };
    atomic(pointerPath, JSON.stringify(next) + "\n");
    try {
      for (const profile of profiles) {
        changed.push({ name: profile.name, previous: originals.get(profile.name) });
        transaction.saveProfile({ ...profile, args: [runner, ...profile.args] }, request.node, cwd, originals.get(profile.name));
        receipt[profile.name] = { command: request.node, cwd, args: [runner, ...profile.args] };
      }
      atomic(receiptPath, JSON.stringify(receipt) + "\n");
    } catch (error) {
      let failed = false;
      for (const original of changed.reverse()) { try { transaction.restoreProfile(original); } catch { failed = true; } }
      try { if (originalReceipt) atomic(receiptPath, originalReceipt); else if (present(receiptPath)) rmSync(receiptPath); } catch { failed = true; }
      if (failed) throw new Error("Profile rollback incomplete; retained usable candidate pointer, inspect profiles before retry");
      if (pointer) atomic(pointerPath, JSON.stringify(pointer) + "\n"); else rmSync(pointerPath);
      throw error;
    }
    return { schema: "treer.launcher-install-result/v1", operation: request.operation, version, source_commit: verified.manifest.source_commit, previous: next.previous,
      compatibility: aligned, profiles: profiles.map((profile) => profile.name), command: request.node, entry: runner, portable: verified.portable };
  } finally { if (stage) rmSync(stage, { recursive: true, force: true }); rmdirSync(lock); }
}
