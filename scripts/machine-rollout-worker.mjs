// Runs only in an explicitly submitted private rollout job, outside Controller exec.
import { createHash, randomUUID } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, renameSync, rmdirSync, writeFileSync, openSync, fsyncSync, closeSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { protectDirectory, syncDirectory } from "../launchers/agent/scripts/private-state.mjs";

const execute = promisify(execFile);
// Only repository-owned fixed diagnostics may leave subprocess output. Never
// persist a stderr excerpt: it can contain startup credentials, paths or argv.
export const diagnosticCodes = ["activation_requires_independent_local_terminal_outside_windows_job", "cannot_verify_update_process_lifetime", "update_worker_failed_inspect_receipt", "update_activation_failed_inspect_receipt", "worker_failed_before_activation", "controller_not_ready_before_update", "installed_and_running_controller_differ", "startup_store_missing", "staged_binary_mismatch", "local_identity_mismatch", "shared_binary_multi_workspace_refused", "require_local_detached_install", "require_default_local_config_directory", "invalid_binary_version", "version_timeout", "update_reparse_path_refused", "out_of_band_manifest_hash_mismatch", "controller_readiness_or_agent_preservation_failed", "restart_command_failed", "restart_command_timeout"];
export function subprocessDiagnostic(error) {
  const summary = value => ({ codes: diagnosticCodes.filter(code => new RegExp(`\\b${code}\\b`).test(String(value || ""))), bytes: Buffer.byteLength(String(value || "")), contentOmitted: true });
  return { exitCode: Number.isInteger(error.code) ? error.code : null, timedOut: error.killed === true,
    signal: ["SIGTERM", "SIGKILL", "SIGINT"].includes(error.signal) ? error.signal : null,
    stdout: summary(error.stdout), stderr: summary(error.stderr) };
}
export const digest = bytes => createHash("sha256").update(bytes).digest("hex");
export const plainEntry = path => path.replace(/^\\\\\?\\([A-Za-z]:\\)/, "$1");
export function requirePhysical(path) {
  if (!isAbsolute(path)) throw new Error("absolute_path_required");
  let current = resolve(path);
  for (;;) {
    if (lstatSync(current).isSymbolicLink()) throw new Error("symlink_path_refused");
    const parent = dirname(current); if (parent === current) break; current = parent;
  }
  return realpathSync.native(path);
}
export function relativeCwd(root, path) {
  const cwd = relative(root, path);
  if (!cwd || isAbsolute(cwd) || cwd === ".." || cwd.startsWith(".." + sep)) throw new Error("smoke_directory_outside_machine_root");
  return cwd;
}
export function save(path, value) {
  const temporary = path + "." + randomUUID() + ".tmp";
  const fd = openSync(temporary, "wx", 0o600);
  try { writeFileSync(fd, JSON.stringify(value) + "\n"); fsyncSync(fd); } finally { closeSync(fd); }
  renameSync(temporary, path); syncDirectory(dirname(path));
}
export function cleanEnvironment(target, current = process.env) {
  const env = {};
  if (target.platform.startsWith("windows-")) {
    for (const key of ["OS", "SystemRoot", "windir", "USERPROFILE", "USERNAME", "USERDOMAIN", "APPDATA", "LOCALAPPDATA", "ProgramData", "HOMEDRIVE", "HOMEPATH", "ComSpec", "PATHEXT", "TEMP", "TMP"]) {
      const found = Object.entries(current).find(([name]) => name.toLowerCase() === key.toLowerCase());
      if (found) env[key] = found[1];
    }
    if (!env.SystemRoot || !env.LOCALAPPDATA) throw new Error("windows_environment_missing");
    env.PATH = [dirname(target.controller), dirname(target.node), join(env.SystemRoot, "System32")].join(";");
  } else {
    env.HOME = target.home; env.USER = target.home.split("/").at(-1);
    env.PATH = [dirname(target.controller), dirname(target.cli), dirname(target.node), "/usr/bin", "/bin", "/usr/sbin", "/sbin"].join(":");
    env.LANG = target.platform.startsWith("darwin-") ? "en_US.UTF-8" : "C.UTF-8";
    if (target.configHome) env.XDG_CONFIG_HOME = target.configHome;
  }
  return env;
}
export async function command(argv, env, timeout = 180000) {
  try {
    return (await execute(plainEntry(argv[0]), argv.slice(1), {
      env, timeout, maxBuffer: 4 * 1024 * 1024, windowsHide: true,
    })).stdout.trim();
  } catch (error) {
    // Never persist arbitrary subprocess output, argv, config or provider logs.
    throw Object.assign(new Error(`subprocess_failed:${error.killed ? "timeout" : Number.isInteger(error.code) ? error.code : "spawn"}`), { subprocess: subprocessDiagnostic(error) });
  }
}
export function windowsWorkerScript(updater, stage, env) {
  const literal = value => "'" + value.replaceAll("'", "''") + "'";
  const variables = Object.entries(env).map(([key, value]) => `[Environment]::SetEnvironmentVariable(${literal(key)},${literal(value)},'Process')`).join(";");
  return `$ErrorActionPreference='Stop';foreach($key in @([Environment]::GetEnvironmentVariables('Process').Keys)){[Environment]::SetEnvironmentVariable($key,$null,'Process')};${variables};` +
    `[Environment]::SetEnvironmentVariable('PSExecutionPolicyPreference','Bypass','Process');[Environment]::SetEnvironmentVariable('PSModulePath',[IO.Path]::Combine($PSHOME,'Modules'),'Process');` +
    `$exitCode=0;$code=$null;try{& ${literal(updater)} -Action Worker -StageDirectory ${literal(stage)} *> $null}catch{$exitCode=1;if(@(${diagnosticCodes.map(literal).join(",")}) -ccontains $_.Exception.Message){$code=$_.Exception.Message}};` +
    `$result=@{exitCode=$exitCode;stderr=@{codes=@($code|Where-Object{$null -ne $_});contentOmitted=$true}}|ConvertTo-Json -Depth 4;` +
    `$file=${literal(join(stage, "worker-exit.json"))};$temp=$file+'.'+[Guid]::NewGuid().ToString('N')+'.tmp';[IO.File]::WriteAllText($temp,$result,[Text.UTF8Encoding]::new($false));[IO.File]::Move($temp,$file);exit $exitCode`;
}
export async function windowsWorker(updater, stage, env, timeout = 600000) {
  // PS5.1 invocation via & does not consistently accept drive verbatim paths.
  // This changes spelling only; the physical/private root was already checked.
  updater = plainEntry(updater); stage = plainEntry(stage);
  const shell = join(env.SystemRoot, "System32/WindowsPowerShell/v1.0/powershell.exe");
  const quote = value => '"' + value.replace(/(\\*)"/g, '$1$1\\"').replace(/(\\+)$/, '$1$1') + '"';
  const literal = value => "'" + value.replaceAll("'", "''") + "'";
  const line = quote(shell) + " -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand " + Buffer.from(windowsWorkerScript(updater, stage, env), "utf16le").toString("base64");
  // Node/libuv can put spawned children in a Job even if Node was launched by
  // CIM. Launch the activation shell itself through CIM with explicit breakaway;
  // keep Assert-UpdateOutsideJob as the final authority, never bypass it.
  const script = `$ErrorActionPreference='Stop';$s=([wmiclass]'Win32_ProcessStartup').CreateInstance();$s.CreateFlags=16777216;$r=([wmiclass]'Win32_Process').Create(${literal(line)},${literal(stage)},$s);if($r.ReturnValue -ne 0){throw 'independent_worker_start_failed'};[Console]::Write($r.ProcessId)`;
  const pid = Number(await command([shell, "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")], env, 20000));
  if (!Number.isInteger(pid) || pid <= 0) throw new Error("independent_worker_pid_missing");
  save(join(stage, "worker-launch.json"), { pid });
  const end = Date.now() + timeout, file = join(stage, "worker-exit.json");
  while (Date.now() < end) {
    if (existsSync(file)) {
      const exit = JSON.parse(readFileSync(file, "utf8"));
      const codes = (Array.isArray(exit.stderr?.codes) ? exit.stderr.codes : []).filter(code => diagnosticCodes.includes(code));
      if (exit.exitCode !== 0) throw Object.assign(new Error("subprocess_failed:1"), { subprocess: { exitCode: 1, pid, stderr: { codes, contentOmitted: true } } });
      return;
    }
    await new Promise(done => setTimeout(done, 250));
  }
  throw Object.assign(new Error("subprocess_failed:timeout"), { subprocess: { exitCode: null, timedOut: true, pid, stderr: { codes: [], contentOmitted: true } } });
}
export function parseSums(bytes) {
  const rows = {};
  for (const line of bytes.toString("ascii").trimEnd().split("\n")) {
    const match = /^([a-f0-9]{64}) [ *](treer(?:-agent-(?:server|host))?)\r?$/.exec(line);
    if (!match || Object.hasOwn(rows, match[2])) throw new Error("invalid_checksum_manifest");
    rows[match[2]] = match[1];
  }
  if (Object.keys(rows).length !== 3) throw new Error("incomplete_checksum_manifest");
  return rows;
}
export async function download(url, limit = 8192) {
  const uri = new URL(url);
  if (uri.protocol !== "https:" || uri.username || uri.password || uri.search || uri.hash) throw new Error("trusted_https_origin_required");
  const response = await fetch(uri, { redirect: "error", signal: AbortSignal.timeout(20000) });
  if (!response.ok) throw new Error("artifact_download_failed");
  const chunks = []; let size = 0;
  for await (const chunk of response.body) {
    size += chunk.length; if (size > limit) throw new Error("artifact_size_limit"); chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}
export async function smoke(target, cli, timeout, run = command, observe = () => {}, cwd = ".") {
  const name = "rollout-smoke-" + randomUUID();
  observe({ name, status: "creating" });
  let id;
  try {
    const value = JSON.parse(await run([...cli, "agent", "admin", "create", "--kind", "command", "--name", name,
      "--machine", target.serverId, "--folder", target.smokeFolder || "/", "--cwd", cwd, "--",
      "treer-agent", "--harness", target.harness, ...(target.harness === "acp" ? ["--acp-command", target.acpCommand] : [])], undefined, timeout));
    id = value.agent_id || value.agent?.agent_id;
    if (!/^ag_[A-Za-z0-9_-]+$/.test(id || "")) throw new Error("smoke_agent_id_missing");
    observe({ id, name, status: "created" });
    const end = Date.now() + timeout;
    while (Date.now() < end) {
      const shown = JSON.parse(await run([...cli, "agent", "show", id], undefined, 10000));
      const agent = shown.agent || shown;
      if (agent.status === "idle" && agent.interface?.protocol === "treer.agent-interface/v1") return { id, status: "idle_with_interface", deleted: true };
      if (["exited", "failed", "stopped"].includes(agent.status)) throw new Error("smoke_agent_exited");
      await new Promise(done => setTimeout(done, 500));
    }
    throw new Error("smoke_agent_timeout");
  } finally {
    // Never delete by name or select another Agent if creation is uncertain.
    if (id) { await run([...cli, "agent", "admin", "delete", id], undefined, 20000); observe({ id, name, status: "deleted" }); }
  }
}

export async function worker(jobPath, { runCommand = command, fetchArtifact = download, runWindowsWorker = windowsWorker } = {}) {
  const job = JSON.parse(readFileSync(jobPath, "utf8")), target = job.target;
  const directory = dirname(requirePhysical(jobPath)), lock = join(directory, ".running");
  if (existsSync(join(directory, "result.json"))) throw new Error("job_already_finished");
  mkdirSync(lock, { mode: 0o700 });
  const receipt = { release: job.commit, machine: target.machine, workspace: target.workspace,
    serverId: target.serverId, pid: process.pid, hostChanged: false, phase: "preflight", status: "running" };
  const progress = () => save(join(directory, "progress.json"), receipt);
  try {
    const env = cleanEnvironment(target, job.environment || process.env);
    for (const key of Object.keys(process.env)) delete process.env[key];
    Object.assign(process.env, env);
    progress();
    if (Number(process.versions.node.split(".")[0]) < 22) throw new Error("node_22_required");
    const actualPlatform = { win32: "windows", darwin: "darwin", linux: "linux" }[process.platform] + "-" + ({ arm64: "aarch64", x64: "x86_64" }[process.arch]);
    if (target.platform !== actualPlatform) throw new Error("machine_platform_mismatch");
    if (target.platform.startsWith("windows-")) checkWindowsPaths(directory, target);
    const smokeCwd = relativeCwd(requirePhysical(job.machineRoot), join(directory, "smoke"));
    for (const key of ["controller", "cli", "config", "startupStore"]) requirePhysical(target[key]);
    const node = realpathSync.native(target.node);
    if (node !== realpathSync.native(process.execPath)) throw new Error("trusted_node_mismatch");
    target.node = node; // A trusted Node installation may use a versioned current junction.
    const service = JSON.parse(readFileSync(target.config, "utf8"));
    if (service.workspace !== target.workspace || service.server_id !== target.serverId) throw new Error("selected_identity_mismatch");
    const local = new URL("http://" + service.listen);
    if (!["127.0.0.1", "[::1]"].includes(local.hostname) || local.username || local.password) throw new Error("local_loopback_required");
    // Bind every following CLI/launcher call to this enrollment, never rediscover.
    env.TREER_WORKSPACE_ID = target.workspace; env.TREER_SERVER_ID = target.serverId; env.TREER_AGENT_SERVER_URL = local.origin;
    const run = (argv, _ignored, timeout) => runCommand(argv, env, timeout);
    const endpoint = job.proxy + "/artifacts/" + target.platform + "/";
    const sums = await fetchArtifact(endpoint + "SHA256SUMS");
    const actual = parseSums(sums);
    if (digest(sums) !== job.sumsHash || Object.keys(job.files).length !== 3 || Object.keys(job.files).some(name => actual[name] !== job.files[name])) throw new Error("release_manifest_mismatch");
    receipt.phase = "core_update"; progress();
    if (target.platform.startsWith("windows-")) {
      const shell = join(env.SystemRoot, "System32/WindowsPowerShell/v1.0/powershell.exe");
      const updater = join(directory, "scripts/update-machine.ps1"), stage = join(directory, "windows-stage");
      const ps = [shell, "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", updater];
      await run([...ps, "-Action", "Prepare", "-Commit", job.sourceCommit, "-SumsSha256", job.sumsHash,
        "-ConfigPath", target.config, "-BinDirectory", dirname(target.controller), "-StageDirectory", stage,
        "-StartupStore", target.startupStore, "-ArtifactBase", endpoint, "-TimeoutSeconds", "180",
        ...(target.acceptSharedBinaries === true ? ["-AcceptSharedBinaries"] : [])], undefined, 120000);
      for (const name of ["treer-agent-server", "treer"]) await run([...ps, "-Action", "Download", "-StageDirectory", stage, "-Binary", name], undefined, 120000);
      await runWindowsWorker(updater, stage, env);
      const result = JSON.parse(readFileSync(join(stage, "result.json")));
      if (result.status !== "complete") throw new Error("core_update_incomplete");
      receipt.backup = result.backup;
    } else {
      const configDir = dirname(target.config);
      const coreDirectory = join(directory, "core"); mkdirSync(coreDirectory, { mode: 0o700 });
      save(join(coreDirectory, "job.json"), { home: target.home, bin_dir: dirname(target.controller), cli_path: target.cli,
        config_dir: configDir, workspace: target.workspace, host: false, accept_shared_binaries: target.acceptSharedBinaries === true,
        proxy: job.proxy, platform: target.platform, timeout: 180, startup_store: target.startupStore,
        expected_sums: job.sumsHash, expected_files: job.files, expected_commit: job.sourceCommit });
      await run([target.python, join(directory, "scripts/update-machine.py"), "--worker", coreDirectory], undefined, 600000);
      receipt.coreUpdated = true;
      const result = JSON.parse(readFileSync(join(coreDirectory, "result.json")));
      if (result.status !== "complete") throw new Error("core_update_incomplete");
      receipt.backup = result.backup;
    }
    receipt.phase = "launcher_install"; receipt.coreUpdated = true; progress();
    await run([target.controller, "launcher", "--workspace", target.workspace, "--url", local.origin,
      "--node", target.node, "--treer", target.cli, "install"], undefined, 360000);
    const smokeDirectory = join(directory, "smoke"); mkdirSync(smokeDirectory, { mode: 0o700 });
    receipt.phase = "smoke"; progress();
    receipt.smoke = await smoke(target, [target.cli], 90000, run, value => { receipt.smoke = value; progress(); }, smokeCwd);
    receipt.phase = "complete"; receipt.status = "complete";
  } catch (error) {
    if (error.subprocess) receipt.subprocess = error.subprocess;
    for (const file of [join(directory, "core/result.json"), join(directory, "windows-stage/result.json")]) {
      if (!existsSync(file)) continue;
      try {
        const core = JSON.parse(readFileSync(file));
        receipt.coreStatus = core.status; receipt.backup = core.backup;
        if (core.status === "complete" || core.status === "rollback_failed") receipt.coreUpdated = true;
      } catch { /* The durable core receipt remains available for inspection. */ }
    }
    // A killed child without a terminal transaction receipt may have activated
    // the candidate. Do not describe an unconfirmed rollback as an untouched failure.
    if (receipt.phase === "core_update" && !receipt.coreStatus) receipt.coreStatus = "unconfirmed";
    receipt.status = receipt.coreUpdated || receipt.coreStatus === "unconfirmed" ? "partial" : "failed";
    receipt.error = /^[A-Za-z0-9_:]+$/.test(error.message) ? error.message.slice(0, 160) : "operation_failed_inspect_private_job";
  } finally {
    save(join(directory, "result.json"), receipt);
    rmdirSync(lock);
  }
  return receipt;
}
export function checkWindowsPaths(directory, target) {
  // PS5.1/.NET framework file APIs are not consistently long-path capable.
  // Account for the longest generated backup/retained-image path before any activation.
  const generated = [join(directory, "backup-" + "a".repeat(40) + "-" + "a".repeat(32), "treer-agent-host.exe"),
    target.controller + ".prev-" + "a".repeat(40) + "-" + "a".repeat(32)];
  if ([directory, ...Object.values(target).filter(value => typeof value === "string"), ...generated]
      .some(path => plainEntry(path).length >= 260)) throw new Error("windows_path_too_long_use_shorter_machine_root");
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  worker(process.argv[2]).then(result => { console.log(JSON.stringify(result)); process.exitCode = result.status === "complete" ? 0 : 1; })
    .catch(() => { console.error("rollout_worker_failed_inspect_private_job"); process.exitCode = 1; });
}
