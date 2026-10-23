import { createHash } from "node:crypto";
import { closeSync, existsSync, lstatSync, openSync, readFileSync, readSync, realpathSync } from "node:fs";
import { isAbsolute, join, parse, relative, resolve, sep, toNamespacedPath } from "node:path";

const fail = () => { throw new Error("Native launcher binding is missing, incompatible or changed; no Node fallback"); };
function physical(path, directory = false) {
  if (typeof path !== "string" || !isAbsolute(path) || resolve(path) !== path || /[\x00-\x1f\x7f]/.test(path)) fail();
  let parent = parse(path).root;
  for (const part of relative(parent, path).split(sep).filter(Boolean)) {
    parent = join(parent, part);
    const info = lstatSync(parent);
    if (info.isSymbolicLink() || process.getuid && ![0, process.getuid()].includes(info.uid) || process.platform !== "win32" && info.mode & 0o022 && !(info.isDirectory() && info.mode & 0o1000)) fail();
  }
  const info = lstatSync(path);
  if (directory ? !info.isDirectory() : !info.isFile()) fail();
  if (toNamespacedPath(realpathSync.native(path)) !== toNamespacedPath(path)) fail();
  return info;
}
function image(binding, executable = false) {
  if (!binding || !/^[a-f0-9]{64}$/.test(binding.sha256 || "") || !Number.isSafeInteger(binding.bytes) || binding.bytes < 1 || binding.bytes > 512 * 1024 * 1024) fail();
  const info = physical(binding.path);
  if (info.size !== binding.bytes || executable && process.platform !== "win32" && !(info.mode & 0o111)) fail();
  const hash = createHash("sha256"), buffer = Buffer.alloc(1024 * 1024), fd = openSync(binding.path, "r");
  try {
    let n, first = true;
    while ((n = readSync(fd, buffer, 0, buffer.length, null)) > 0) {
      if (first && executable) {
        const magic = buffer.subarray(0, 4).toString("hex");
        if (!(process.platform === "linux" ? magic === "7f454c46" : process.platform === "win32" ? magic.startsWith("4d5a") : ["cffaedfe", "feedfacf", "cafebabe", "bebafeca"].includes(magic))) fail();
      }
      first = false; hash.update(buffer.subarray(0, n));
    }
  }
  finally { closeSync(fd); }
  if (hash.digest("hex") !== binding.sha256) fail();
}
function readBinding(root) {
  const info = physical(join(root, "native-runtime.json"));
  if (info.size > 65536) fail();
  return JSON.parse(readFileSync(join(root, "native-runtime.json"), "utf8"));
}
export function nativeRuntime(root, harnesses = ["codex", "claude"]) {
  const value = readBinding(root);
  if (value.schema !== "treer.native-launcher/v1" || value.platform !== process.platform || value.arch !== process.arch || !/^[a-f0-9]{40}$/.test(value.source_commit || "") || !Array.isArray(value.assets) || value.assets.length < 1 || value.assets.length > 64) fail();
  image(value.executable, true); physical(value.public_dir, true);
  const names = new Set();
  for (const asset of value.assets) {
    const rel = relative(value.public_dir, asset.path || "");
    if (!rel || isAbsolute(rel) || rel === ".." || rel.startsWith(`..${sep}`) || names.has(rel)) fail();
    names.add(rel); image(asset);
  }
  for (const name of ["index.html", "boot.js", "ui.js", "ui.css"]) if (!names.has(name)) fail();
  // New split UIs declare their entire transitive asset closure. Old four-file
  // UIs remain compatible; new manifests cannot omit a worker/shared chunk.
  const inventoryPath = join(value.public_dir, "ui-assets.json");
  if (existsSync(inventoryPath)) {
    if (!names.has("ui-assets.json") || physical(inventoryPath).size > 65536) fail();
    const inventory = JSON.parse(readFileSync(inventoryPath, "utf8"));
    if (inventory.schema !== "treer.ui-assets/v1" || !Array.isArray(inventory.files) || inventory.files.length > 63) fail();
    const declared = new Set();
    for (const entry of inventory.files) {
      if (typeof entry.path !== "string" || !/^[A-Za-z0-9_.-]+$/.test(entry.path) || declared.has(entry.path)) fail();
      declared.add(entry.path);
      const asset = value.assets.find(asset => relative(value.public_dir, asset.path) === entry.path);
      if (!asset || asset.bytes !== entry.bytes || asset.sha256 !== entry.sha256) fail();
    }
    if (declared.size + 1 !== names.size) fail();
  }
  for (const tool of ["treer", ...harnesses]) {
    if (!value.tools || typeof value.tools[tool]?.version !== "string" || !value.tools[tool].version) fail();
    image(value.tools[tool], true);
  }
  return value;
}
export function runtimeStatus(root) {
  if (!existsSync(join(root, "native-runtime.json"))) {
    const nativeRequired = existsSync(join(root, "scripts/native-runtime.mjs"));
    return { default: nativeRequired ? "unconfigured" : "node", codex: nativeRequired ? "unconfigured" : "node", claude: nativeRequired ? "unconfigured" : "node", acp: "explicit-node" };
  }
  const metadata = readBinding(root);
  const value = nativeRuntime(root, ["codex", "claude"].filter(name => metadata.tools?.[name]));
  return { default: "rust", codex: value.tools.codex ? "rust" : "unavailable-provider", claude: value.tools.claude ? "rust" : "unavailable-provider", acp: "explicit-node", source_commit: value.source_commit, executable: value.executable, platform: value.platform, arch: value.arch, providers: Object.fromEntries(Object.entries(value.tools).map(([name, tool]) => [name, { path: tool.path, version: tool.version, sha256: tool.sha256 }])) };
}
