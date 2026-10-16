import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync, existsSync, chmodSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { Script } from "node:vm";
import { jobsFor, submit, validateTarget, windowsQuote } from "./machine-rollout.mjs";
import { checkWindowsPaths, cleanEnvironment, digest, parseSums, smoke, worker } from "./machine-rollout-worker.mjs";

const commit = "a".repeat(40), sourceCommit = "b".repeat(40);
const platform = { darwin: "darwin", linux: "linux", win32: "windows" }[process.platform] + "-" + ({ arm64: "aarch64", x64: "x86_64" }[process.arch]);
const target = { machine: "fixture", workspace: "ws_fixture", serverId: "srv_fixture", platform: "linux-x86_64",
  node: "/trusted/node", python: "/trusted/python3", controller: "/trusted/treer-agent-server", cli: "/trusted/treer",
  config: "/private/config.json", startupStore: "/private/srv_fixture-agent-startup.json", home: "/private/home", harness: "claude" };
const sums = Buffer.from(["treer", "treer-agent-server", "treer-agent-host"].map((name, i) => String(i + 1).repeat(64) + "  " + name + "\n").join(""));
function manifest() {
  return { commit, status: "canary_passed", machine_artifacts: { source_commit: sourceCommit, files: Object.fromEntries([
    ...Object.entries(parseSums(sums)).map(([name, hash]) => ["linux-x86_64/" + name, hash]), ["linux-x86_64/SHA256SUMS", digest(sums)]]) } };
}
test("offline plan requires exact eligible manifest, all pins, explicit targets and no duplicates", () => {
  const jobs = jobsFor(commit, manifest(), [target], "https://proxy.invalid");
  assert.equal(jobs[0].sourceCommit, sourceCommit); assert.equal(jobs[0].sumsHash, digest(sums));
  assert.throws(() => jobsFor(commit.slice(0, 7), manifest(), [target], "https://proxy.invalid"));
  assert.throws(() => jobsFor(commit, { ...manifest(), status: "pending" }, [target], "https://proxy.invalid"));
  assert.throws(() => jobsFor(commit, manifest(), [target, target], "https://proxy.invalid"));
  assert.throws(() => jobsFor(commit, manifest(), [target], "https://user:secret@proxy.invalid"));
  assert.throws(() => jobsFor(commit, manifest(), [target], "http://localhost"));
  const missing = manifest(); delete missing.machine_artifacts.files["linux-x86_64/treer-agent-host"];
  assert.throws(() => jobsFor(commit, missing, [target], "https://proxy.invalid"));
  assert.throws(() => validateTarget({ ...target, node: "node" }));
  assert.throws(() => validateTarget({ ...target, harness: "acp", acpCommand: "provider" }));
});
test("default CLI planning is offline and does not execute treer or create a receipt", () => {
  const directory = realpathSync.native(mkdtempSync(join(tmpdir(), "treer-rollout-plan-")));
  try {
    const manifestFile = join(directory, "manifest.json"), targets = join(directory, "targets.json"), receipt = join(directory, "not-created", "receipt.json");
    writeFileSync(manifestFile, JSON.stringify(manifest())); writeFileSync(targets, JSON.stringify([target]));
    const output = execFileSync(process.execPath, [join(import.meta.dirname, "machine-rollout.mjs"), "--sha", commit,
      "--manifest", manifestFile, "--targets", targets, "--proxy", "https://offline.invalid", "--receipt", receipt,
      "--treer", join(directory, "missing-treer")], { encoding: "utf8", env: { SystemRoot: process.env.SystemRoot, LOCALAPPDATA: process.env.LOCALAPPDATA, HOME: directory } });
    assert.equal(JSON.parse(output).changes, false); assert.equal(existsSync(join(directory, "not-created")), false);
  } finally { rmSync(directory, { recursive: true }); }
});
test("Windows argv and clean environment retain OS prerequisites but exclude all identity/proxy/provider settings", () => {
  const windows = { ...target, platform: "windows-x86_64", node: "C:\\trusted\\node.exe", controller: "C:\\bin\\treer-agent-server.exe" };
  const env = cleanEnvironment(windows, { SystemRoot: "C:\\Windows", LOCALAPPDATA: "C:\\Users\\fixture\\AppData\\Local",
    USERPROFILE: "C:\\Users\\fixture", TREER_WORKLOAD_CREDENTIAL: "secret", OPENAI_API_KEY: "secret", HTTP_PROXY: "secret", NODE_OPTIONS: "secret", PATH: "untrusted" });
  assert.equal(env.SystemRoot, "C:\\Windows");
  assert.equal(Object.values(env).some(value => value.includes("secret") || value.includes("untrusted")), false);
  assert.equal(windowsQuote("C:\\space folder\\"), '"C:\\space folder\\\\"');
  assert.equal(windowsQuote('a"b'), '"a\\"b"');
  const unix = cleanEnvironment(target, { TREER_AGENT_ID: "old", AGENT_UI_SESSION_ID: "old", HTTP_PROXY: "old" });
  assert.deepEqual(Object.keys(unix).sort(), ["HOME", "LANG", "PATH", "USER"]);
  checkWindowsPaths("C:\\short", windows);
  assert.throws(() => checkWindowsPaths("C:\\" + "x".repeat(180), windows), /windows_path_too_long/);
});
test("checksums reject duplicate, extra or malformed filenames", () => {
  assert.throws(() => parseSums(Buffer.concat([sums, sums])));
  assert.throws(() => parseSums(Buffer.from("0".repeat(64) + "  ../treer\n")));
});
test("smoke checks AIS idle, deletes only returned ID even when it exits, and does not delete an uncertain create", async () => {
  const calls = [];
  const run = async argv => { calls.push(argv); if (argv.includes("create")) return '{"agent_id":"ag_smoke"}';
    if (argv.includes("show")) return '{"status":"exited"}'; return "{}"; };
  await assert.rejects(smoke(target, ["treer"], 1000, run), /smoke_agent_exited/);
  assert.deepEqual(calls.at(-1).slice(-4), ["agent", "admin", "delete", "ag_smoke"]);
  calls.length = 0;
  await assert.rejects(smoke(target, ["treer"], 1000, async argv => { calls.push(argv); throw new Error("create_reply_lost"); }));
  assert.equal(calls.length, 1);
});
test("worker enforces pinned sums, core-before-launcher ordering, partial receipt, bound CLI and cleanup", async () => {
  const directory = realpathSync.native(mkdtempSync(join(tmpdir(), "treer-rollout-worker-"))), original = { ...process.env };
  try {
    for (const mode of ["mismatch", "complete", "partial", "core_unconfirmed", "environment_missing"]) {
      const jobDir = join(directory, mode); mkdirSync(jobDir);
      const current = { ...target, platform, home: directory, controller: join(directory, "controller"), cli: join(directory, "cli"), node: realpathSync.native(process.execPath),
        python: join(directory, "python"), config: join(directory, "config.json"), startupStore: join(directory, "startup.json") };
      for (const path of [current.controller, current.cli, current.python, current.startupStore]) writeFileSync(path, "fixture");
      writeFileSync(current.config, JSON.stringify({ workspace: target.workspace, server_id: target.serverId, listen: "127.0.0.1:12345", operator_credential: "synthetic-not-for-logs" }));
      const job = { commit, sourceCommit, target: current, files: parseSums(sums), sumsHash: mode === "mismatch" ? "0".repeat(64) : digest(sums), proxy: "https://proxy.invalid" };
      if (mode === "environment_missing") { job.target.platform = "windows-x86_64"; job.environment = {}; }
      const file = join(jobDir, "rollout.json"); writeFileSync(file, JSON.stringify(job));
      const calls = [];
      process.env.TREER_WORKLOAD_CREDENTIAL = "must-be-cleared";
      const result = await worker(file, { fetchArtifact: async () => sums, runCommand: async (argv, env) => {
        calls.push(argv); assert.equal(env.TREER_WORKSPACE_ID, target.workspace); assert.equal(env.TREER_SERVER_ID, target.serverId);
        assert.equal(env.TREER_WORKLOAD_CREDENTIAL, undefined); assert.equal(env.HTTP_PROXY, undefined);
        if (argv.includes("--worker")) {
          if (mode === "core_unconfirmed") throw new Error("subprocess_failed:timeout");
          assert.equal(existsSync(join(jobDir, "result.json")), false, "core completion must not become overall completion");
          writeFileSync(join(jobDir, "core/result.json"), JSON.stringify({ status: "complete", backup: "private-backup" })); return "";
        }
        if (argv.includes("-Action")) {
          const action = argv[argv.indexOf("-Action") + 1], stage = join(jobDir, "windows-stage");
          if (action === "Prepare") mkdirSync(stage);
          if (action === "Worker") {
            if (mode === "core_unconfirmed") throw new Error("subprocess_failed:timeout");
            assert.equal(existsSync(join(jobDir, "result.json")), false);
            writeFileSync(join(stage, "result.json"), JSON.stringify({ status: "complete", backup: "private-backup" }));
          }
          return "{}";
        }
        if (argv.includes("launcher")) { if (mode === "partial") throw new Error("launcher_failed"); return "{}"; }
        if (argv.includes("create")) {
          assert.equal(argv[argv.indexOf("--cwd") + 1], join(jobDir, "smoke"));
          assert.deepEqual(readdirSync(join(jobDir, "smoke")), []);
          return '{"agent_id":"ag_smoke"}';
        }
        if (argv.includes("show")) return '{"status":"idle","interface":{"protocol":"treer.agent-interface/v1"}}';
        return "{}";
      } });
      assert.equal(result.hostChanged, false); assert.equal(existsSync(join(jobDir, ".running")), false);
      if (["mismatch", "environment_missing"].includes(mode)) { assert.equal(result.status, "failed"); assert.equal(calls.length, 0); }
      else if (mode === "core_unconfirmed") { assert.equal(result.status, "partial"); assert.equal(result.coreStatus, "unconfirmed"); assert.equal(calls.some(argv => argv.includes("launcher")), false); }
      else {
        assert.equal(result.status, mode);
        if (process.platform === "win32") { assert.ok(calls[0].includes("Prepare")); assert.ok(calls[3].includes("Worker")); assert.ok(calls[4].includes("launcher")); }
        else { assert.equal(calls[0][0], current.python); assert.ok(calls[1].includes("launcher")); }
        if (mode === "complete") assert.ok(calls.at(-1).includes("delete"));
      }
      assert.equal(readFileSync(join(jobDir, "result.json"), "utf8").includes("synthetic-not-for-logs"), false);
    }
  } finally { for (const key of Object.keys(process.env)) delete process.env[key]; Object.assign(process.env, original); rmSync(directory, { recursive: true }); }
});
test("generated detached submission scripts compile on both platforms; pointer is saved before launch", { skip: process.platform === "win32" }, async () => {
  const directory = realpathSync.native(mkdtempSync(join(tmpdir(), "treer-rollout-submit-")));
  try {
    const cli = join(directory, "treer");
    writeFileSync(cli, `#!${process.execPath}
const fs=require('node:fs'),vm=require('node:vm');const args=process.argv.slice(2);
if(args[0]==='exec'){const code=args.at(-1);new vm.Script(code);fs.appendFileSync(${JSON.stringify(join(directory, "scripts.jsonl"))},JSON.stringify(code)+'\\n');
const started=code.includes('worker_not_started');if(started&&!fs.existsSync(${JSON.stringify(join(directory, "pointer"))}))process.exit(3);
console.log(JSON.stringify({exit_code:0,stdout:JSON.stringify({directory:${JSON.stringify(directory)},environment:{},pid:started?1234:undefined})}));}
else if(args[0]!=='machine')process.exit(2);
`); chmodSync(cli, 0o700);
    for (const current of [target, { ...target, platform: "windows-x86_64" }]) {
      const value = await submit({ target: current, commit }, cli, () => writeFileSync(join(directory, "pointer"), "saved"));
      assert.equal(value.pid, 1234);
    }
    const codes = readFileSync(join(directory, "scripts.jsonl"), "utf8").trim().split("\n").map(JSON.parse);
    assert.equal(codes.length, 4); for (const code of codes) new Script(code);
    assert.ok(codes[3].includes("Win32_Process")); assert.ok(codes[3].includes("detached:true"));
  } finally { rmSync(directory, { recursive: true }); }
});
