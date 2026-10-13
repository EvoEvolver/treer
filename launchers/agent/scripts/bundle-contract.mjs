import { createHash } from "node:crypto";
import { lstatSync, readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

export const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
export function regular(path, limit = 32 * 1024 * 1024) {
  const info = lstatSync(path);
  if (!info.isFile() || info.isSymbolicLink() || info.size > limit) throw new Error("Invalid or oversized launcher file");
  return readFileSync(path);
}
export function compatibility(contract, controller) {
  if (!controller || !Array.isArray(controller.capabilities) || !Array.isArray(controller.features)) return { status: "unknown", reason: "Controller capability handshake unavailable" };
  if (contract.schema !== "treer.launcher-interface/v1" || contract.protocol !== controller.protocol || !Array.isArray(contract.capabilities) || !Array.isArray(contract.required_features)) return { status: "incompatible", reason: "Launcher protocol/contract mismatch" };
  if (!contract.capabilities.every((capability) => controller.capabilities.includes(capability)) || !contract.required_features.every((feature) => controller.features.includes(feature))) return { status: "incompatible", reason: "Controller lacks launcher capabilities/features; update Controller before installing this launcher" };
  return { status: "compatible", reason: null };
}
export function verifyBundle(directory, version, sourceCommit, allowDependencies = false) {
  if (lstatSync(directory).isSymbolicLink() || !lstatSync(directory).isDirectory()) throw new Error("Bundle must be a physical directory");
  const raw = regular(join(directory, "bundle-manifest.json"), 2 * 1024 * 1024);
  if (version !== `sha256-${digest(raw)}`) throw new Error("Launcher content version mismatch");
  const manifest = JSON.parse(raw.toString("utf8"));
  if (manifest.schema !== "treer.launcher-payload/v1" || !/^[a-f0-9]{40}$/.test(manifest.source_commit || "") || sourceCommit && sourceCommit !== manifest.source_commit || !Array.isArray(manifest.files)) throw new Error("Invalid launcher bundle manifest");
  const entries = new Map();
  for (const file of manifest.files) {
    if (typeof file.path !== "string" || !file.path || file.path.includes("\\") || file.path.includes(":") || file.path.startsWith("/") || file.path.split("/").some((part) => ["", ".", ".."].includes(part)) || !/^[a-f0-9]{64}$/.test(file.sha256 || "") || !Number.isSafeInteger(file.size) || file.size < 0 || typeof file.executable !== "boolean" || entries.has(file.path) || file.path === "bundle-manifest.json") throw new Error("Invalid launcher file manifest entry");
    entries.set(file.path, file);
  }
  const found = new Set();
  function visit(path, prefix = "") {
    for (const name of readdirSync(path)) {
      const relative = prefix + name, full = join(path, name), info = lstatSync(full);
      if (info.isSymbolicLink() || !info.isDirectory() && !info.isFile()) throw new Error("Launcher bundle contains a link or special file");
      if (relative === "node_modules" && info.isDirectory() && allowDependencies && manifest.runtime_ready !== true) continue;
      if (info.isDirectory()) visit(full, relative + "/");
      else if (relative !== "bundle-manifest.json") {
        const expected = entries.get(relative), bytes = regular(full);
        if (!expected || bytes.length !== expected.size || digest(bytes) !== expected.sha256) throw new Error("Launcher file integrity mismatch");
        found.add(relative);
      }
    }
  }
  visit(directory);
  if (found.size !== entries.size) throw new Error("Launcher bundle files missing");
  for (const name of ["interface-contract.json", "profiles.json", "scripts/install.mjs", "scripts/installed-runner.mjs", "scripts/treer-agent.mjs"]) if (!found.has(name)) throw new Error("Launcher bundle entry points missing");
  return { manifest, contract: JSON.parse(regular(join(directory, "interface-contract.json")).toString("utf8")), portable: manifest.runtime_ready === true && found.has("runtime.mjs") };
}
