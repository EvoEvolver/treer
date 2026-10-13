import { chmodSync, closeSync, fsyncSync, lstatSync, mkdirSync, mkdtempSync, openSync, readdirSync, realpathSync, renameSync, rmSync, rmdirSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import { compatibility, regular, verifyBundle } from "./bundle-contract.mjs";
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
  if (Number(process.versions.node.split(".")[0]) < 22 || realpathSync(request.node) !== realpathSync(process.execPath)) throw new Error("Installer requires the selected Node.js 22+ executable");
  const physicalRoot = realpathSync(request.root);
  if (physicalRoot !== resolve(request.root)) throw new Error("Machine root must be a physical path");
  const cwd = request.cwd ?? ".", cwdInside = relative(physicalRoot, realpathSync(resolve(physicalRoot, cwd)));
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
      return { schema: "treer.launcher-install-result/v1", operation: "status", current: pointer?.version ?? null, previous: pointer?.previous ?? null, versions,
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
        }
        renameSync(candidate, versionRoot);
      }
      // Reuse only verified immutable directories, never overwrite running assets.
      if (verified.portable) verifyBundle(versionRoot, version, request.source_commit);
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
    const changed = [];
    const runner = join(root, "run.mjs");
    // The stable trampoline has no version-specific logic and is never replaced
    // during an update; all runtime assets remain in immutable version trees.
    if (!present(runner)) atomic(runner, regular(join(versionRoot, "scripts/installed-runner.mjs")));
    else regular(runner);
    const next = { schema: "treer.launcher-current/v1", version, previous: pointer?.version !== version ? pointer?.version ?? null : pointer?.previous ?? null, node: request.node, treer: request.treer };
    atomic(pointerPath, JSON.stringify(next) + "\n");
    try {
      for (const profile of profiles) {
        changed.push({ name: profile.name, previous: originals.get(profile.name) });
        transaction.saveProfile({ ...profile, args: [runner, ...profile.args] }, request.node, cwd, originals.get(profile.name));
      }
    } catch (error) {
      let failed = false;
      for (const original of changed.reverse()) { try { transaction.restoreProfile(original); } catch { failed = true; } }
      if (failed) throw new Error("Profile rollback incomplete; retained usable candidate pointer, inspect profiles before retry");
      if (pointer) atomic(pointerPath, JSON.stringify(pointer) + "\n"); else rmSync(pointerPath);
      throw error;
    }
    return { schema: "treer.launcher-install-result/v1", operation: request.operation, version, source_commit: verified.manifest.source_commit, previous: next.previous,
      compatibility: aligned, profiles: profiles.map((profile) => profile.name), command: request.node, entry: runner, portable: verified.portable };
  } finally { if (stage) rmSync(stage, { recursive: true, force: true }); rmdirSync(lock); }
}
