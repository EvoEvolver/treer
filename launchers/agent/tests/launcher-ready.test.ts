import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { hermeticEnv } from "./hermetic.mjs";
import { launcherReady } from "./launcher-ready.js";

test("readiness never returns an empty/invalid address when startup misses its deadline", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "launcher-ready-"));
  const alive = { exitCode: null, signalCode: null } as ReturnType<typeof spawn>;
  try {
    for (const value of [null, "", "0", "65536", "not-a-port"]) {
      const file = join(cwd, `port-${String(value)}`);
      if (value !== null) await writeFile(file, value);
      await assert.rejects(launcherReady(alive, file, 15), /readiness timed out/);
    }
    await assert.rejects(launcherReady({ exitCode: 1, signalCode: null } as ReturnType<typeof spawn>, join(cwd, "missing")), /exited before/);
  } finally { await rm(cwd, { recursive: true, force: true }); }
});

test("a delayed listening address waits for harness readiness before returning a usable base URL", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "launcher-ready-")), file = join(cwd, "port");
  // Real subprocess publishes its ephemeral listener after a delay, then changes
  // health from503 to200. No fixed port or provider installation is involved.
  const child = spawn(process.execPath, ["--input-type=module", "-e", `
    import {createServer} from 'node:http'; import {writeFileSync} from 'node:fs';
    let ready=false; const server=createServer((req,res)=>{res.writeHead(ready?200:503);res.end()});
    setTimeout(()=>server.listen(0,'127.0.0.1',()=>{writeFileSync(process.argv[1],String(server.address().port));setTimeout(()=>{ready=true},100)}),100);
  `, file], { env: hermeticEnv(), stdio: "ignore" });
  try {
    const base = await launcherReady(child, file);
    assert.match(base, /^http:\/\/127\.0\.0\.1:[1-9]\d*$/);
    assert.equal((await fetch(`${base}/api/health`)).status, 200);
  } finally {
    const exited = child.exitCode === null && child.signalCode === null ? new Promise(resolve => child.once("exit", resolve)) : Promise.resolve();
    child.kill(); await exited;
    await rm(cwd, { recursive: true, force: true });
  }
});
