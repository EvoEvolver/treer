import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { providerCommand, spawnProvider, stopProcessTree } from "../scripts/process.mjs";
import { hermeticEnv } from "./hermetic.mjs";

test("Windows npm cmd providers resolve to Node + JS without shell expansion", async t => {
  const root = mkdtempSync(join(tmpdir(), "provider with space-")); t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, "node_modules/provider/bin"); mkdirSync(bin, { recursive: true });
  const entry = join(bin, "cli.mjs"), shim = join(root, "codex.cmd");
  writeFileSync(entry, "console.log(JSON.stringify(process.argv.slice(2)))");
  writeFileSync(shim, '@ECHO off\r\n"%_prog%" "%dp0%\\node_modules\\provider\\bin\\cli.mjs" %*\r\n');
  const args = ["space value", "hello world $ &", '"quote" | ^ % !', "中文"];
  const resolved = providerCommand(shim, args, { platform: "win32" });
  assert.equal(resolved.command, process.execPath); assert.deepEqual(resolved.args.slice(1), args);
  const child = spawnProvider(resolved.command, resolved.args, { env: hermeticEnv(), stdio: "pipe" });
  let output = ""; child.stdout.on("data", chunk => { output += chunk; });
  const [code] = await once(child, "exit"); assert.equal(code, 0); assert.deepEqual(JSON.parse(output), args);
  assert.deepEqual(providerCommand("codex", args, { platform: "win32", env: { PATH: root } }), resolved);
  writeFileSync(shim, 'cmd.exe /c "untrusted arbitrary command"');
  assert.throws(() => providerCommand(shim, [], { platform: "win32" }), /Unsupported provider/);
  assert.throws(() => providerCommand("missing-codex", [], { platform: "win32", env: { PATH: root } }), /not on PATH/);
});

test("native exe paths remain argv vectors and ambiguous npm shims fail closed", t => {
  assert.deepEqual(providerCommand(process.execPath, ["--version"], { platform: "win32" }), { command: process.execPath, args: ["--version"] });
  const root = mkdtempSync(join(tmpdir(), "provider-ambiguous-")); t.after(() => rmSync(root, { recursive: true, force: true }));
  const shim = join(root, "claude.cmd");
  writeFileSync(shim, '"%dp0%\\node_modules\\one\\cli.js"\n"%dp0%\\node_modules\\two\\cli.js"');
  assert.throws(() => providerCommand(shim, [], { platform: "win32" }), /Unsupported provider/);
});

test("stopping a provider removes its child process tree", async t => {
  const root = mkdtempSync(join(tmpdir(), "provider-tree-")); t.after(() => rmSync(root, { recursive: true, force: true }));
  const file = join(root, "pids.json"), script = join(root, "tree.mjs");
  writeFileSync(script, "import {spawn} from 'node:child_process';import{writeFileSync}from'node:fs';const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});writeFileSync(process.argv[2],JSON.stringify([process.pid,child.pid]));setInterval(()=>{},1000)");
  const child = spawnProvider(process.execPath, [script, file], { env: hermeticEnv(), stdio: "pipe" });
  t.after(() => stopProcessTree(child, "SIGKILL"));
  const deadline = Date.now() + 10000;
  let pids: number[] = [];
  while (Date.now() < deadline) { try { pids = JSON.parse(readFileSync(file, "utf8")); break; } catch { await new Promise(done => setTimeout(done, 20)); } }
  assert.equal(pids.length, 2);
  const closed = once(child, "close"); stopProcessTree(child); await closed;
  for (const pid of pids) {
    while (Date.now() < deadline) { try { process.kill(pid, 0); } catch { break; } await new Promise(done => setTimeout(done, 20)); }
    assert.throws(() => process.kill(pid, 0), /ESRCH|no such process/i);
  }
});
