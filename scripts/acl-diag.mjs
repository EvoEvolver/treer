// Read-only diagnostic: time protectDirectory (15 s cap) and an uncapped copy on a private temp directory.
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
const source = readFileSync("launchers/agent/scripts/private-state.mjs", "utf8");
const uncappedPath = join("launchers/agent/scripts", "private-state-uncapped.mjs");
writeFileSync(uncappedPath, source.replace("timeout: 15000", "timeout: 180000"));
const modules = { capped: await import(pathToFileURL(join(process.cwd(), "launchers/agent/scripts/private-state.mjs")).href), uncapped: await import(pathToFileURL(join(process.cwd(), uncappedPath)).href) };
for (const run of [1, 2, 3]) for (const name of ["capped", "uncapped"]) {
  const dir = mkdtempSync(join(tmpdir(), "acl-diag-")); const start = Date.now();
  let outcome = "ok"; try { modules[name].protectDirectory(dir); } catch (error) { outcome = "error:" + error.message.slice(0, 60); }
  console.log(`run=${run} variant=${name} outcome=${outcome} ms=${Date.now() - start}`);
  rmSync(dir, { recursive: true, force: true });
}
rmSync(uncappedPath);
