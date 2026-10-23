// Installer/dispatch control fixture only: Node's native executable stands in
// for an operator-attested runtime. This is not Rust/provider live acceptance.
import { createHash } from "node:crypto";
import { existsSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { join } from "node:path";
export function nativeBindingFixture(directory, publicDirectory, executable = realpathSync.native(process.execPath)) {
  const bind = path => { const bytes = readFileSync(path); return { path, bytes: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex") }; };
  const tool = { ...bind(executable), version: "fixture-only" };
  const inventory = join(publicDirectory, "ui-assets.json");
  const assets = existsSync(inventory) ? [...JSON.parse(readFileSync(inventory, "utf8")).files.map(entry => entry.path), "ui-assets.json"] : ["index.html", "boot.js", "ui.js", "ui.css"];
  const value = { schema: "treer.native-launcher/v1", platform: process.platform, arch: process.arch, source_commit: "a".repeat(40), executable: bind(executable), public_dir: publicDirectory,
    assets: assets.map(name => bind(join(publicDirectory, name))), tools: { treer: tool, codex: tool, claude: tool } };
  const path = join(directory, "native-fixture.json"), bytes = JSON.stringify(value);
  writeFileSync(path, bytes, { mode: 0o600 });
  return { path, value, sha256: createHash("sha256").update(bytes).digest("hex") };
}
