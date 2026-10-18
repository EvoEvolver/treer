// The browser suite runs against either launcher implementation (#158 M1: UI parity).
//   LAUNCHER_BACKEND=node (default)  the production Node/tsx launcher
//   LAUNCHER_BACKEND=rust            the Rust launcher binary named by RUST_LAUNCHER_BIN (a build with the fixture-harness
//                                    feature, so CODEX_BIN can point at the fake Codex)
//   SUITE_REPORT=1                   do not stop at the first failing step: run every step and print a failure list
// The Node path stays byte-for-byte the former behaviour (fail fast, same spawn).
import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
import { join } from "node:path";

export const backend = process.env.LAUNCHER_BACKEND || "node";
if (!["node", "rust"].includes(backend)) throw new Error(`LAUNCHER_BACKEND must be node or rust, got ${backend}`);
if (backend === "rust" && !process.env.RUST_LAUNCHER_BIN) throw new Error("LAUNCHER_BACKEND=rust needs RUST_LAUNCHER_BIN (absolute path of the fixture-harness build)");
export const reportMode = process.env.SUITE_REPORT === "1";

/** Harnesses the backend implements; a server for another one cannot start and its steps are reported as unsupported. */
export const supports = (harness) => backend === "node" || harness === "codex";

/** Spawn the launcher for one fixture server. `env` is the fully prepared hermetic environment of the Node launcher. */
export function spawnLauncher(app, env) {
  if (backend === "node") return spawn(join(app, "node_modules/.bin/tsx"), [join(app, "src/index.ts")], { env, stdio: "pipe" });
  // The Rust launcher reads the AGENT_UI_* names (the Node launcher also accepts CODEX_UI_*), serves the same public/ assets
  // and takes the fake Codex through CODEX_BIN (fixture-harness build).
  const translated = { ...env };
  for (const [from, to] of [["CODEX_UI_CWD", "AGENT_UI_CWD"], ["CODEX_UI_PORT_FILE", "AGENT_UI_PORT_FILE"], ["CODEX_UI_PUBLIC_DIR", "AGENT_UI_PUBLIC_DIR"], ["CODEX_UI_PORT", "AGENT_UI_PORT"]]) if (translated[from] !== undefined && translated[to] === undefined) translated[to] = translated[from];
  translated.AGENT_UI_PUBLIC_DIR ??= join(app, "public");
  return spawn(process.env.RUST_LAUNCHER_BIN, [], { env: translated, cwd: translated.AGENT_UI_CWD, stdio: "pipe" });
}

/** Run one named step. Default: plain call (a failure stops the suite). Report mode: record the failure and go on. */
export function createSteps() {
  const results = [];
  let dirty = false;
  const detail = (error) => ({ detail: String(error?.message ?? error).split("\n").slice(0, 14).join("\n"), at: String(error?.stack ?? "").split("\n").find((line) => /tests[\\/]/.test(line))?.trim().replace(/^at /, "") });
  const firstLine = (error) => String(error?.message ?? error).split("\n").find((line) => line.trim()) ?? String(error);
  return {
    results,
    /** `recover` runs before the step that follows a failure, to put the page back into a known state. */
    async step(name, fn, recover) {
      if (!reportMode) return fn();
      if (dirty && recover) { try { await recover(); } catch { /* the next step reports it */ } }
      dirty = false;
      try { await fn(); results.push({ name, status: "pass" }); console.log(`PASS ${name}`); }
      catch (error) {
        dirty = true;
        if (error?.unsupported) { results.push({ name, status: "unsupported", message: firstLine(error) }); console.log(`SKIP ${name}: ${firstLine(error)}`); return; }
        results.push({ name, status: "fail", message: firstLine(error), ...detail(error) }); console.log(`FAIL ${name}: ${firstLine(error)}`);
      }
    },
    skip(name, reason) { if (reportMode) { results.push({ name, status: "unsupported", message: reason }); console.log(`SKIP ${name}: ${reason}`); } },
    summary() {
      const failed = results.filter((item) => item.status === "fail"), skipped = results.filter((item) => item.status === "unsupported");
      console.log(`\n[${backend}] ${results.length - failed.length - skipped.length} passed, ${failed.length} failed, ${skipped.length} unsupported`);
      for (const item of [...failed, ...skipped]) console.log(`  ${item.status.toUpperCase()}  ${item.name}: ${item.message}${item.at ? `  [${item.at}]` : ""}`);
      if (process.env.SUITE_REPORT_FILE) writeFileSync(process.env.SUITE_REPORT_FILE, JSON.stringify({ backend, results }, null, 2));
      return failed.length;
    },
  };
}
