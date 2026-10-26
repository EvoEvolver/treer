import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { compatibility, digest, regular, verifyBundle } from "./bundle-contract.mjs";
import { nativeRuntime, runtimeStatus } from "./native-runtime.mjs";
import { fetchObject, LATEST_URL, objectUrl, releasePlatform, validateFiles } from "./latest-download.mjs";

const MAX_ASSET = 256 * 1024 * 1024;
const canonical = value => JSON.stringify(value, Object.keys(value).sort());
function bytes(row, limit) {
  if (!row || !Number.isSafeInteger(row.size) || row.size < 0 || row.size > limit || !/^[a-f0-9]{64}$/.test(row.sha256 || "") || typeof row.data_base64 !== "string" || row.data_base64.length !== Math.ceil(row.size / 3) * 4) throw new Error("Invalid launcher release file encoding");
  const data = Buffer.from(row.data_base64, "base64");
  if (data.length !== row.size || data.toString("base64") !== row.data_base64 || digest(data) !== row.sha256) throw new Error("Launcher release file checksum mismatch");
  return data;
}

export async function latestUpdate({ url = LATEST_URL, root, treer, before, controller, deliver, selected, report }) {
  objectUrl(url);
  const platform = releasePlatform(), signal = AbortSignal.timeout(180000);
  // Resolve the mutable channel once; every later byte is bound to this snapshot.
  const channel = JSON.parse((await fetchObject(url, 65536, signal)).toString("utf8"));
  if (channel.schema !== "treer.launcher-latest/v1" || !/^[a-f0-9]{40}$/.test(channel.source_commit || "") || !channel.platforms || typeof channel.platforms !== "object") throw new Error("Invalid launcher latest channel");
  const published = channel.platforms[platform];
  if (!published) throw new Error(`Latest launcher release unavailable for ${platform}`);
  const row = { sha256: published.package_sha256, size: published.package_size, url: published.package_url };
  if (!/^[a-f0-9]{64}$/.test(row.sha256 || "") || !Number.isSafeInteger(row.size) || row.size < 1 || row.size > MAX_ASSET) throw new Error("Invalid launcher platform release descriptor");
  const assetUrl = objectUrl(row.url), channelUrl = objectUrl(url);
  if (channelUrl.protocol === "https:" && (assetUrl.origin !== channelUrl.origin || assetUrl.pathname !== `/EvoEvolver/treer/releases/download/launcher-${channel.source_commit}/launcher-${platform}.json`)) throw new Error("Launcher platform object is not bound to the frozen release source");
  if (channelUrl.protocol === "http:" && assetUrl.origin !== channelUrl.origin) throw new Error("Fixture release object changed origin");
  const base = join(root, ".treer/launchers/agent"), current = before.current;
  if (!/^sha256-[a-f0-9]{64}$/.test(current || "")) throw new Error("Latest update requires an existing verified Rust installation with local provider bindings");
  const currentDirectory = join(base, current);
  verifyBundle(currentDirectory, current);
  const metadata = JSON.parse(regular(join(currentDirectory, "native-runtime.json"), 65536));
  const binding = nativeRuntime(currentDirectory, ["codex", "claude"].filter(name => metadata.tools?.[name]));
  report({ operation: "update", stage: "latest-resolved", current, target: channel.source_commit, platform, release_sha256: row.sha256, running_agents: "unchanged" });
  try {
    const origin = JSON.parse(regular(join(currentDirectory, "release-origin.json"), 65536));
    if (origin.schema === "treer.launcher-release-origin/v1" && origin.sha256 === row.sha256 && origin.source_commit === channel.source_commit && origin.platform === platform) return { schema: "treer.launcher-update-result/v1", current, target: current, changed: false, status: "unchanged", runtime: before.current_runtime, release_source: channel.source_commit, running_agents: "unchanged" };
  } catch (error) { if (error.code !== "ENOENT") throw error; }
  const raw = await fetchObject(assetUrl, row.size, signal);
  if (raw.length !== row.size || digest(raw) !== row.sha256) throw new Error("Launcher platform release checksum mismatch");
  const release = JSON.parse(raw.toString("utf8"));
  if (release.schema !== "treer.launcher-package/v1" || release.source_commit !== channel.source_commit || release.platform !== platform || !/^[a-f0-9]{40}$/.test(release.native?.source_commit || "") || !release.native.tools || canonical(Object.keys(release.native.tools).sort()) !== canonical(Object.keys(binding.tools).sort())) throw new Error("Invalid or incompatible launcher platform release");
  for (const name of Object.keys(binding.tools)) if (canonical(release.native.tools[name]) !== canonical(binding.tools[name])) throw new Error(`Latest native release requires a different local ${name} tool binding; no provider was downloaded or changed`);
  validateFiles({ schema: "treer.launcher-payload/v1", source_commit: release.source_commit, files: release.files });
  if (release.native.path !== "native/treer-agent" || release.files.reduce((n, file) => n + file.size, 0) > 192 * 1024 * 1024 || release.files.some(file => file.path !== release.native.path && !file.path.startsWith("payload/"))) throw new Error("Invalid launcher release payload closure");
  // Verify the entire decoded closure before creating a local release directory.
  const all = release.files.map(file => ({ file, data: bytes(file, 32 * 1024 * 1024) }));
  const nativeRow = all.find(({ file }) => file.path === release.native.path);
  const originalManifest = all.find(({ file }) => file.path === "payload/bundle-manifest.json");
  if (!nativeRow?.data.length || !originalManifest || digest(originalManifest.data) !== release.payload_manifest_sha256) throw new Error("Missing native image or mismatched payload manifest");
  const native = nativeRow.data, payload = JSON.parse(originalManifest.data);
  validateFiles(payload);
  if (payload.source_commit !== release.source_commit || payload.runtime_ready !== true || payload.node_min !== 22 || payload.entry !== "runtime.mjs") throw new Error("Invalid portable launcher payload manifest");
  const decoded = all.filter(({ file }) => file.path.startsWith("payload/") && file.path !== "payload/bundle-manifest.json").map(({ file, data }) => ({ file: { ...file, path: file.path.slice(8) }, data }));
  if (decoded.length !== payload.files.length || decoded.some(({ file }) => {
    const expected = payload.files.find(item => item.path === file.path);
    return !expected || ["native-runtime.json", "release-origin.json"].includes(file.path) || file.path === "native" || file.path.startsWith("native/") || expected.size !== file.size || expected.sha256 !== file.sha256 || expected.executable !== file.executable;
  })) throw new Error("Launcher release payload manifest does not describe the exact closure");
  const contract = decoded.find(({ file }) => file.path === "interface-contract.json");
  const aligned = compatibility(contract && JSON.parse(contract.data), controller);
  if (aligned.status !== "compatible") throw new Error(aligned.reason);
  // Permanent private custody is necessary: native-runtime contains absolute
  // paths and running sessions must retain their selected image/public assets.
  const directory = mkdtempSync(join(base, ".release-")); let installAttempted = false;
  try {
    const files = [];
    const put = (path, data, executable = false) => {
      const destination = join(directory, path); mkdirSync(dirname(destination), { recursive: true, mode: 0o700 });
      writeFileSync(destination, data, { flag: "wx", mode: executable ? 0o700 : 0o600 });
      files.push({ path, size: data.length, sha256: digest(data), executable });
    };
    for (const { file, data } of decoded) put(file.path, data, file.executable);
    const nativeName = `native/treer-agent-rust${process.platform === "win32" ? ".exe" : ""}`;
    put(nativeName, native, true);
    const assets = files.filter(file => /^public\/[A-Za-z0-9_.-]+$/.test(file.path)).map(file => ({ path: join(directory, file.path), bytes: file.size, sha256: file.sha256 }));
    put("native-runtime.json", Buffer.from(JSON.stringify({ schema: "treer.native-launcher/v1", platform: process.platform, arch: process.arch, source_commit: release.native.source_commit, executable: { path: join(directory, nativeName), bytes: native.length, sha256: nativeRow.file.sha256 }, public_dir: join(directory, "public"), assets, tools: binding.tools })));
    put("release-origin.json", Buffer.from(JSON.stringify({ schema: "treer.launcher-release-origin/v1", source_commit: release.source_commit, sha256: row.sha256, platform })));
    files.sort((a, b) => a.path.localeCompare(b.path, "en"));
    const manifest = Buffer.from(JSON.stringify({ schema: "treer.launcher-payload/v1", source_commit: release.source_commit, node_min: 22, entry: "runtime.mjs", runtime_ready: true, files }, null, 2) + "\n");
    writeFileSync(join(directory, "bundle-manifest.json"), manifest, { flag: "wx", mode: 0o600 });
    const sha256 = digest(manifest), target = `sha256-${sha256}`;
    verifyBundle(directory, target); runtimeStatus(directory);
    report({ operation: "update", stage: "verified-candidate", current, target, release_source: release.source_commit, running_agents: "unchanged" });
    installAttempted = true;
    const result = await deliver(["update", ...selected, "--bundle", directory, "--sha256", sha256, "--require-runtime", "rust"]);
    return { ...result, schema: "treer.launcher-update-result/v1", current, target: result.version, changed: current !== result.version, status: "updated", release_source: release.source_commit, release_sha256: row.sha256, running_agents: "unchanged", note: "Only the default entry changed. Existing sessions and providers were not restarted or installed." };
  } catch (error) {
    if (!installAttempted) rmSync(directory, { recursive: true, force: true });
    else report({ operation: "update", stage: "install-result-unknown-or-refused", retained_directory: directory, note: "Inspect status before retrying; native assets retained in case publication succeeded." });
    throw error;
  }
}
