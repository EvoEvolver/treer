// Installer/dispatch control fixture only: Node's native executable stands in
// for an operator-attested runtime. This is not Rust/provider live acceptance.
import { createHash } from "node:crypto";
import { chmodSync, constants, copyFileSync, existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { join } from "node:path";
export function nativeBindingFixture(directory, publicDirectory, executable = realpathSync.native(process.execPath)) {
  // CI toolcaches/checkouts may have group-writable ancestors. Bind only our
  // own physical copies, never loosen nativeRuntime or chmod shared inputs.
  const inputs = join(directory, "native-inputs"), stagedPublic = join(inputs, "public");
  mkdirSync(inputs, { mode: 0o700 }); mkdirSync(stagedPublic, { mode: 0o700 });
  const stagedExecutable = join(inputs, process.platform === "win32" ? "control.exe" : "control");
  copyFileSync(executable, stagedExecutable, constants.COPYFILE_EXCL);
  chmodSync(stagedExecutable, 0o700);
  const bind = path => { const bytes = readFileSync(path); return { path, bytes: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex") }; };
  const tool = { ...bind(stagedExecutable), version: "fixture-only" };
  const inventory = join(publicDirectory, "ui-assets.json");
  const assets = existsSync(inventory) ? [...JSON.parse(readFileSync(inventory, "utf8")).files.map(entry => entry.path), "ui-assets.json"] : ["index.html", "boot.js", "ui.js", "ui.css"];
  for (const name of assets) {
    if (typeof name !== "string" || !/^[A-Za-z0-9_.-]+$/.test(name) || name === "." || name === "..") throw new Error("Invalid fixture asset");
    const target = join(stagedPublic, name);
    copyFileSync(join(publicDirectory, name), target, constants.COPYFILE_EXCL);
    chmodSync(target, 0o600);
  }
  const value = { schema: "treer.native-launcher/v1", platform: process.platform, arch: process.arch, source_commit: "a".repeat(40), executable: bind(stagedExecutable), public_dir: stagedPublic,
    assets: assets.map(name => bind(join(stagedPublic, name))), tools: { treer: tool, codex: tool, claude: tool } };
  const path = join(directory, "native-fixture.json"), bytes = JSON.stringify(value);
  writeFileSync(path, bytes, { mode: 0o600 });
  return { path, value, sha256: createHash("sha256").update(bytes).digest("hex") };
}
