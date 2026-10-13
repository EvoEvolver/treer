#!/usr/bin/env node
import { build } from "esbuild";
import { cpSync, existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const source = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const options = {};
for (let i = 2; i < process.argv.length; i += 2) {
  if (!["--output", "--source-commit"].includes(process.argv[i]) || !process.argv[i + 1]) throw new Error("Usage: prepare-bundle.mjs --output ABSOLUTE_DIR --source-commit FULL_SHA");
  options[process.argv[i].slice(2)] = process.argv[i + 1];
}
if (!isAbsolute(options.output || "") || !/^[a-f0-9]{40}$/.test(options["source-commit"] || "")) throw new Error("An absolute output and full source commit are required");
const output = options.output;
if (existsSync(output) && (lstatSync(output).isSymbolicLink() || !lstatSync(output).isDirectory() || readdirSync(output).length)) throw new Error("Output must be a new or empty physical directory");
mkdirSync(output, { recursive: true, mode: 0o700 });
for (const name of ["public", "scripts", "profiles.json", "interface-contract.json"]) cpSync(join(source, name), join(output, name), { recursive: true, dereference: false });
await build({ entryPoints: [join(source, "src/index.ts")], outfile: join(output, "runtime.mjs"), bundle: true,
  platform: "node", format: "esm", target: "node22", minify: false,
  external: ["bufferutil", "utf-8-validate"], define: { "process.env.WS_NO_BUFFER_UTIL": '"1"', "process.env.WS_NO_UTF_8_VALIDATE": '"1"' },
  banner: { js: 'import {createRequire as __treerCreateRequire} from "node:module"; const require=__treerCreateRequire(import.meta.url);' } });
const files = [];
function visit(directory) {
  for (const name of readdirSync(directory).sort()) {
    const path = join(directory, name), info = lstatSync(path);
    if (info.isSymbolicLink() || !info.isDirectory() && !info.isFile()) throw new Error("Payload contains a link or special file");
    if (info.isDirectory()) visit(path);
    else { const bytes = readFileSync(path); files.push({ path: relative(output, path).split("\\").join("/"), size: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex"), executable: Boolean(info.mode & 0o111) }); }
  }
}
visit(output); files.sort((a, b) => a.path.localeCompare(b.path, "en"));
writeFileSync(join(output, "bundle-manifest.json"), JSON.stringify({ schema: "treer.launcher-payload/v1", source_commit: options["source-commit"], node_min: 22, entry: "runtime.mjs", runtime_ready: true, files }, null, 2) + "\n");
console.log(JSON.stringify({ output, files: files.length, source_commit: options["source-commit"] }));
