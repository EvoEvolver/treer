// Download transport for launcher releases. Installation and compatibility
// remain the existing bundle installer's responsibility.

export const MAX_BUNDLE_BYTES = 256 * 1024 * 1024;
export const LATEST_URL = "https://github.com/EvoEvolver/treer/releases/download/launcher-stable/launcher-latest.json";
const MAX_FILE_BYTES = 32 * 1024 * 1024;

export function releasePlatform(platform = process.platform, arch = process.arch) {
  const os = { darwin: "darwin", linux: "linux", win32: "windows" }[platform];
  const cpu = { x64: "x86_64", arm64: "aarch64" }[arch];
  if (!os || !cpu) throw new Error("Latest launcher release does not support this OS/architecture");
  return `${os}-${cpu}`;
}

export function releaseBase(value) {
  const url = new URL(value);
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  if (url.protocol !== "https:" && !(url.protocol === "http:" && loopback) || url.username || url.password || url.search || url.hash || !url.pathname.endsWith("/")) throw new Error("Release base must be HTTPS without credentials, query or fragment (HTTP loopback only for tests)");
  return url;
}

export function releasePath(path) {
  if (typeof path !== "string" || !path || path.length > 1024 || !path.split("/").every(part => /^[A-Za-z0-9._-]+$/.test(part) && part !== "." && part !== "..")) throw new Error("Invalid release object path");
  return path;
}

export async function fetchRelease(base, path, limit, signal) {
  return fetchObject(new URL(releasePath(path), base), limit, signal);
}

export function objectUrl(value) {
  const url = new URL(value);
  if (url.username || url.password || url.hash) throw new Error("Invalid launcher release URL");
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  if (loopback && url.protocol === "http:") return url;
  if (url.protocol !== "https:" || url.port || url.hostname !== "github.com" || !url.pathname.startsWith("/EvoEvolver/treer/releases/download/") || url.search) throw new Error("Launcher release URL must select the official GitHub release repository");
  return url;
}

export async function fetchObject(value, limit, signal) {
  let url = objectUrl(value), response;
  for (let redirect = 0; ; redirect++) {
    response = await fetch(url, { redirect: "manual", signal, headers: { "Accept-Encoding": "identity" }, cache: "no-store" });
    if (![301, 302, 303, 307, 308].includes(response.status)) break;
    await response.body?.cancel();
    const location = response.headers.get("location");
    if (!location || redirect >= 2 || url.hostname !== "github.com") throw new Error("Launcher release redirect refused");
    const next = new URL(location, url);
    if (next.protocol !== "https:" || next.hostname !== "release-assets.githubusercontent.com" || next.port || next.username || next.password || next.hash) throw new Error("Launcher release redirect refused");
    url = next;
  }
  if (!response.ok) { await response.body?.cancel(); throw new Error(`Launcher release download failed (HTTP ${response.status}); no fallback was performed`); }
  const declared = response.headers.get("content-length");
  if (declared !== null && (!/^\d+$/.test(declared) || Number(declared) > limit)) { await response.body?.cancel(); throw new Error("Launcher release object exceeds download limit"); }
  const chunks = []; let size = 0;
  try {
    for await (const chunk of response.body || []) {
      size += chunk.length;
      if (size > limit) throw new Error("Launcher release object exceeds download limit");
      chunks.push(chunk);
    }
  } catch (error) { throw new Error("Launcher release download interrupted", { cause: error }); }
  if (declared !== null && size !== Number(declared)) throw new Error("Launcher release download length mismatch");
  return Buffer.concat(chunks, size);
}

export function validateFiles(manifest) {
  if (manifest?.schema !== "treer.launcher-payload/v1" || !/^[a-f0-9]{40}$/.test(manifest.source_commit || "") || !Array.isArray(manifest.files) || !manifest.files.length || manifest.files.length > 1024) throw new Error("Invalid launcher release manifest");
  const seen = new Set(); let total = 0;
  for (const file of manifest.files) {
    releasePath(file.path);
    if (file.path === "bundle-manifest.json" || seen.has(file.path) || !/^[a-f0-9]{64}$/.test(file.sha256 || "") || !Number.isSafeInteger(file.size) || file.size < 0 || file.size > MAX_FILE_BYTES || typeof file.executable !== "boolean") throw new Error("Invalid launcher release file");
    seen.add(file.path); total += file.size;
    if (total > MAX_BUNDLE_BYTES) throw new Error("Launcher release bundle exceeds download limit");
  }
  // Reject file/directory collisions before downloading anything.
  for (const path of seen) for (let i = path.indexOf("/"); i !== -1; i = path.indexOf("/", i + 1)) if (seen.has(path.slice(0, i))) throw new Error("Conflicting launcher release paths");
}

