#!/usr/bin/env node
// Leader-operated rollout: planning is offline; --apply explicitly changes machines.
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { readFileSync, mkdirSync, existsSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve, win32, posix } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { digest, plainEntry, requirePhysical, save } from "./machine-rollout-worker.mjs";
import { protectDirectory } from "../launchers/agent/scripts/private-state.mjs";
const exec = promisify(execFile), root = resolve(import.meta.dirname, "..");
// Array form is for closed cross-platform test fixtures (node + fake CLI script).
const runTreer = (treer, argv, options) => {
  const [binary, ...prefix] = Array.isArray(treer) ? treer : [treer];
  return exec(binary, [...prefix, ...argv], options);
};
const assets = ["scripts/machine-rollout-worker.mjs", "scripts/update-machine.py", "scripts/update-machine.ps1", "launchers/agent/scripts/private-state.mjs"];
export function validateTarget(target) {
  const fields = new Set(["machine", "workspace", "serverId", "platform", "node", "python", "controller", "cli", "config", "startupStore", "home", "harness", "acpCommand", "smokeFolder", "configHome", "acceptSharedBinaries"]);
  if (target && Object.keys(target).some(key => !fields.has(key))) throw new Error("unknown_target_field_refused");
  if (!target || !/^srv_[A-Za-z0-9_-]{1,96}$/.test(target.serverId || "") ||
      !/^ws_[A-Za-z0-9_-]{1,96}$/.test(target.workspace || "") || !/^[A-Za-z0-9_-]{1,128}$/.test(target.machine || "")) throw new Error("explicit_machine_workspace_server_required");
  if (!/^(darwin|linux)-(aarch64|x86_64)$|^windows-x86_64$/.test(target.platform || "")) throw new Error("unsupported_platform");
  const path = target.platform.startsWith("windows-") ? win32 : posix;
  for (const key of ["node", "controller", "cli", "config", "startupStore", "home", ...(!target.platform.startsWith("windows-") ? ["python"] : [])]) {
    if (typeof target[key] !== "string" || !path.isAbsolute(target[key]) || /[\u0000-\u001f\u007f"]/.test(target[key])) throw new Error("trusted_absolute_paths_required:" + key);
  }
  if (target.configHome && (!path.isAbsolute(target.configHome) || /[\u0000-\u001f\u007f"]/.test(target.configHome))) throw new Error("trusted_config_home_required");
  if (path.basename(target.startupStore) !== target.serverId + "-agent-startup.json") throw new Error("startup_store_must_match_server");
  if (path.basename(target.controller) !== (target.platform.startsWith("windows-") ? "treer-agent-server.exe" : "treer-agent-server")) throw new Error("installed_controller_filename_required");
  if (target.platform.startsWith("windows-") && path.basename(target.cli) !== "treer.exe") throw new Error("installed_windows_cli_filename_required");
  if (target.platform.startsWith("windows-") && path.dirname(target.cli).toLowerCase() !== path.dirname(target.controller).toLowerCase()) throw new Error("windows_binaries_require_same_directory");
  if (!["codex", "claude", "acp"].includes(target.harness)) throw new Error("explicit_smoke_harness_required");
  if (target.harness === "acp" && (!path.isAbsolute(target.acpCommand || "") || /[\u0000-\u001f\u007f"]/.test(target.acpCommand))) throw new Error("trusted_acp_command_required");
  if (target.smokeFolder && (!target.smokeFolder.startsWith("/") || /[\u0000-\u001f\u007f]/.test(target.smokeFolder))) throw new Error("absolute_smoke_tree_folder_required");
  return target;
}
export function jobsFor(commit, manifest, targets, proxy) {
  if (!/^[a-f0-9]{40}$/.test(commit || "") || manifest.commit !== commit || manifest.status !== "canary_passed") throw new Error("exact_canary_passed_manifest_required");
  const uri = new URL(proxy);
  if (uri.protocol !== "https:" || uri.username || uri.password || uri.search || uri.hash || uri.pathname !== "/") throw new Error("trusted_https_proxy_origin_required");
  const sourceCommit = manifest.machine_artifacts?.source_commit;
  if (!/^[a-f0-9]{40}$/.test(sourceCommit || "")) throw new Error("artifact_source_commit_required");
  if (!Array.isArray(targets) || !targets.length) throw new Error("explicit_targets_required");
  const seen = new Set();
  return targets.map(target => {
    validateTarget(target);
    if (seen.has(target.serverId)) throw new Error("duplicate_target"); seen.add(target.serverId);
    const files = {}, hashes = manifest.machine_artifacts.files;
    for (const name of ["treer", "treer-agent-server", "treer-agent-host", "SHA256SUMS"]) {
      const hash = hashes?.[target.platform + "/" + name];
      if (!/^[a-f0-9]{64}$/.test(hash || "")) throw new Error("manifest_artifact_hash_missing");
      if (name !== "SHA256SUMS") files[name] = hash;
    }
    return { commit, sourceCommit, target, proxy: uri.origin, files, sumsHash: hashes[target.platform + "/SHA256SUMS"] };
  });
}
// Windows CommandLineToArgvW quoting, including paths ending in a backslash.
export const windowsQuote = value => '"' + value.replace(/(\\*)"/g, '$1$1\\"').replace(/(\\+)$/g, "$1$1") + '"';
export async function remote(target, code, treer = "treer") {
  let output;
  try {
    output = (await runTreer(treer, ["exec", "--machine", target.machine, "--cwd", ".", "--timeout", "25000", "--",
      plainEntry(target.node), "--eval", code], { timeout: 30000, maxBuffer: 1024 * 1024 })).stdout;
  } catch { throw new Error("remote_exec_failed"); }
  const envelope = JSON.parse(output);
  if (envelope.exit_code !== 0 || envelope.timed_out || envelope.truncated) throw new Error("remote_exec_incomplete");
  return JSON.parse(envelope.stdout);
}
async function upload(target, source, destination, treer) {
  try { await runTreer(treer, ["machine", "put", "--machine", target.machine, "--mode", "600", source, destination], { timeout: 120000, maxBuffer: 65536 }); }
  catch (error) {
    const denied = /machine_file_denied|policy_denied|403 Forbidden/.test(String(error.stderr));
    throw new Error(denied ? "machine_upload_denied_check_policy_and_reserved_paths" : "machine_upload_failed");
  }
}
export async function preflightTransfer(target, treer = "treer") {
  const directory = mkdtempSync(join(tmpdir(), "treer-rollout-probe-")), file = join(directory, "probe"), name = "rollout-probe-" + randomUUID() + ".txt";
  const bytes = Buffer.from("Treer rollout transfer probe\n");
  try {
    writeFileSync(file, bytes, { mode: 0o600 });
    try { await upload(target, file, name, treer); }
    catch (error) { throw new Error(error.message.includes("denied") ? "machine_transfer_preflight_denied_no_setup_check_policy" : "machine_transfer_preflight_failed_no_setup"); }
    try {
      await remote(target, `const fs=require('node:fs'),p=require('node:path'),c=require('node:crypto'),file=p.join(fs.realpathSync.native(process.cwd()),${JSON.stringify(name)});if(!fs.lstatSync(file).isFile()||c.createHash('sha256').update(fs.readFileSync(file)).digest('hex')!==${JSON.stringify(digest(bytes))})throw Error('probe_mismatch');fs.unlinkSync(file);console.log('{}');`, treer);
    } catch { throw new Error("machine_transfer_probe_cleanup_unconfirmed_no_setup_inspect_audit"); }
  } finally { rmSync(directory, { recursive: true }); }
}
export async function submit(job, treer = "treer", onPrepared = () => {}) {
  await preflightTransfer(job.target, treer);
  const relative = "treer-rollouts/" + job.commit + "-" + randomUUID();
  const helper = Buffer.from(readFileSync(join(root, assets[3]))).toString("base64");
  // Only bounded, non-secret OS variables survive exec's inherited Agent env.
  const code = `const fs=require('node:fs'),p=require('node:path');(async()=>{
    const {protectDirectory}=await import('data:text/javascript;base64,${helper}');
    let current=fs.realpathSync.native(process.cwd());
    for(const part of ${JSON.stringify(relative.split("/"))}){
      current=p.join(current,part);if(fs.existsSync(current)){if(fs.lstatSync(current).isSymbolicLink()||!fs.statSync(current).isDirectory())throw Error('unsafe_job_parent');}
      else fs.mkdirSync(current,{mode:448});
    }protectDirectory(current);
    for(const child of ['scripts','launchers','launchers/agent','launchers/agent/scripts'])fs.mkdirSync(p.join(current,child),{mode:448});
    const environment={};for(const [k,v] of Object.entries(process.env))if(/^(OS|SystemRoot|windir|USERPROFILE|USERNAME|USERDOMAIN|APPDATA|LOCALAPPDATA|ProgramData|HOMEDRIVE|HOMEPATH|ComSpec|PATHEXT|TEMP|TMP)$/i.test(k))environment[k]=v;
    console.log(JSON.stringify({directory:current,machineRoot:fs.realpathSync.native(process.cwd()),environment}));})().catch(()=>{process.exitCode=1});`;
  const setup = await remote(job.target, code, treer);
  const path = job.target.platform.startsWith("windows-") ? win32 : posix;
  for (const asset of assets) await upload(job.target, join(root, asset), relative + "/" + asset, treer);
  const verified = Object.fromEntries(assets.map(asset => [asset, digest(readFileSync(join(root, asset)))]));
  const payload = { ...job, machineRoot: setup.machineRoot, environment: setup.environment };
  // Persist the pointer BEFORE launching: a lost exec reply is not a reason to
  // submit another worker. Resume polls this same private directory.
  const prepared = { directory: setup.directory, target: job.target, commit: job.commit };
  await onPrepared(prepared);
  const start = `const fs=require('node:fs'),p=require('node:path'),cp=require('node:child_process'),crypto=require('node:crypto');(async()=>{
    const directory=${JSON.stringify(setup.directory)},job=${JSON.stringify(payload)};
    for(const [file,hash]of Object.entries(${JSON.stringify(verified)}))if(crypto.createHash('sha256').update(fs.readFileSync(p.join(directory,file))).digest('hex')!==hash)throw Error('worker_hash_mismatch');
    const {cleanEnvironment,save,plainEntry}=await import(require('node:url').pathToFileURL(p.join(directory,'scripts/machine-rollout-worker.mjs')));
    const env=cleanEnvironment(job.target,job.environment),file=p.join(directory,'rollout.json');save(file,job);
    const argv=[plainEntry(job.target.node),plainEntry(p.join(directory,'scripts/machine-rollout-worker.mjs')),plainEntry(file)];
    let pid;
    if(process.platform==='win32'){
      const quote=${windowsQuote.toString()};
      const line=argv.map(quote).join(' '),literal=v=>"'"+v.replaceAll("'","''")+"'";
      const script="$ErrorActionPreference='Stop'; $r=([wmiclass]'Win32_Process').Create("+literal(line)+","+literal(plainEntry(directory))+",$null); if($r.ReturnValue -ne 0){throw 'CIM start failed'}; [Console]::Write($r.ProcessId)";
      pid=Number(cp.execFileSync(p.join(env.SystemRoot,'System32/WindowsPowerShell/v1.0/powershell.exe'),['-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(script,'utf16le').toString('base64')],{env,encoding:'utf8',timeout:20000}));
    }else{const child=cp.spawn(argv[0],argv.slice(1),{env,detached:true,stdio:'ignore',cwd:directory});pid=child.pid;child.unref();}
    if(!Number.isInteger(pid)||pid<=0)throw Error('worker_not_started');console.log(JSON.stringify({directory,pid}));})().catch(()=>{process.exitCode=1});`;
  return { ...prepared, ...await remote(job.target, start, treer) };
}
export async function poll(submitted, treer = "treer", timeout = 1200000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    try {
      const result = await remote(submitted.target, `const fs=require('node:fs'),p=require('node:path');const file=p.join(${JSON.stringify(submitted.directory)},'result.json');console.log(fs.existsSync(file)?fs.readFileSync(file,'utf8'):'null');`, treer);
      if (result) return result;
    } catch { /* The selected Controller reconnects; never resubmit the worker. */ }
    await new Promise(done => setTimeout(done, 2000));
  }
  return { status: "pending", machine: submitted.target.machine, directory: submitted.directory, note: "worker continues; use --resume" };
}
async function main() {
  const args = process.argv.slice(2), options = {};
  if (Number(process.versions.node.split(".")[0]) < 22) throw new Error("node_22_required");
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--apply") options.apply = true;
    else if (args[i] === "--dry-run") options.dryRun = true;
    else if (["--sha", "--manifest", "--targets", "--proxy", "--receipt", "--resume", "--treer"].includes(args[i]) && args[i + 1]) options[args[i].slice(2)] = args[++i];
    else throw new Error("usage: node scripts/machine-rollout.mjs --sha FULL_SHA --manifest FILE --targets FILE --proxy HTTPS_ORIGIN [--apply --receipt NEW_FILE] | --resume RECEIPT");
  }
  if (options.apply && options.dryRun) throw new Error("apply_and_dry_run_conflict");
  const treer = options.treer || "treer";
  if (options.resume) {
    const receipt = JSON.parse(readFileSync(options.resume));
    if (receipt.schema !== "treer.machine-rollout/v1" || !Array.isArray(receipt.submitted)) throw new Error("invalid_rollout_receipt");
    for (const item of receipt.submitted) validateTarget(item.target);
    for (const item of receipt.submitted) if (!item.result || item.result.status === "pending") item.result = await poll(item, treer);
    save(options.resume, receipt); console.log(JSON.stringify(receipt, null, 2));
    if (receipt.submitted.length !== receipt.targetCount || receipt.submitted.some(item => item.result?.status !== "complete")) process.exitCode = 1;
    return;
  }
  const jobs = jobsFor(options.sha, JSON.parse(readFileSync(options.manifest)), JSON.parse(readFileSync(options.targets)), options.proxy);
  if (!options.apply) { console.log(JSON.stringify({ status: "plan", changes: false, hostChanged: false, jobs }, null, 2)); return; }
  if (!options.receipt || existsSync(options.receipt)) throw new Error("new_private_receipt_path_required");
  const parent = dirname(resolve(options.receipt));
  if (existsSync(parent)) throw new Error("receipt_requires_new_private_directory");
  mkdirSync(parent, { mode: 0o700 }); requirePhysical(parent); protectDirectory(parent);
  const receipt = { schema: "treer.machine-rollout/v1", commit: options.sha, targetCount: jobs.length, submitted: [] }; save(options.receipt, receipt);
  for (const job of jobs) {
    // Sequential by design: stop on partial/failure rather than upgrading the next machine.
    let item;
    try {
      const started = await submit(job, treer, prepared => { item = prepared; receipt.submitted.push(item); save(options.receipt, receipt); });
      Object.assign(item, started);
    } catch (error) {
      if (!item) throw error;
      item.startAcknowledgement = "unknown"; // Poll, never retry the launch RPC.
    }
    item.result = await poll(item, treer); save(options.receipt, receipt);
    if (item.result.status !== "complete") break;
  }
  console.log(JSON.stringify(receipt, null, 2));
  if (receipt.submitted.length !== jobs.length || receipt.submitted.some(item => item.result.status !== "complete")) process.exitCode = 1;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(/^[A-Za-z0-9_:]+$/.test(error.message) ? error.message : "rollout_failed_inspect_private_receipt"); process.exitCode = 1; });
}
