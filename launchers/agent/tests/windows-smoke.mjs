// Native acceptance: real embedded installer + sealed fake registration/ACP.
// No provider credentials, network install, real Agent creation or restart.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, existsSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { isAbsolute, join, resolve, toNamespacedPath } from "node:path";
import { chromium, expect } from "@playwright/test";
import { hermeticEnv, localFetch } from "./hermetic.mjs";
import { spawnProvider, stopProcessTree } from "../scripts/process.mjs";
import { protectDirectory } from "../scripts/private-state.mjs";
import { digest } from "../scripts/bundle-contract.mjs";

const options = {};
for (let i = 2; i < process.argv.length; i += 2) {
  if (!["--controller", "--screenshots", "--bundle"].includes(process.argv[i]) || !isAbsolute(process.argv[i + 1] || "")) throw new Error("Use --controller ABSOLUTE_EXE --screenshots ABSOLUTE_DIR (or --bundle ABSOLUTE_DIR for JS-only development checks)");
  options[process.argv[i].slice(2)] = process.argv[i + 1];
}
if (Boolean(options.controller) === Boolean(options.bundle) || !options.screenshots) throw new Error("Select exactly one Controller executable or development bundle, plus screenshots");
const launcher = resolve(import.meta.dirname, ".."), windows = process.platform === "win32";
const parent = windows ? process.env.LOCALAPPDATA : resolve(launcher, "../../.treer");
if (!isAbsolute(parent || "")) throw new Error("An absolute LOCALAPPDATA is required on Windows");
mkdirSync(parent, { recursive: true, mode: 0o700 });
const fixture = realpathSync(mkdtempSync(join(parent, "launcher-windows-smoke-")));
protectDirectory(fixture);
// Exercise spaces, Unicode and >260-character installed assets on Windows.
const machine = join(fixture, "machine space 中文", ...Array.from({ length: windows ? 5 : 1 }, (_, i) => `long-${i}-` + "x".repeat(45)));
mkdirSync(machine, { recursive: true, mode: 0o700 });
const home = join(fixture, "home"), temp = join(fixture, "temp");
mkdirSync(home); mkdirSync(temp); mkdirSync(options.screenshots, { recursive: true });
const cli = join(fixture, windows ? "fake-treer.exe" : "fake-treer");
// rustc is a CI/build prerequisite; the installed runtime does not require it.
execFileSync("rustc", [join(launcher, "tests/fake-treer.rs"), "-o", cli], { env: hermeticEnv(), stdio: "pipe" });
const contract = JSON.parse(readFileSync(join(launcher, "interface-contract.json"), "utf8"));
const controller = { git_commit: "a".repeat(40), protocol: contract.protocol, capabilities: contract.capabilities, features: contract.required_features,
  minimum_launcher_security_epoch: contract.launcher_security_epoch, machine: { root: machine, server_id: "srv_smoke", workspace_id: "ws_smoke" } };
writeFileSync(join(fixture, "contract.json"), JSON.stringify(controller));
const env = hermeticEnv({ HOME: home, USERPROFILE: home, APPDATA: home, LOCALAPPDATA: home, TEMP: temp, TMP: temp,
  FIXTURE_ROOT: fixture, FIXTURE_NODE: process.execPath, FIXTURE_TREER_SCRIPT: join(launcher, "tests/fake-treer.mjs"), PATH: "",
  TREER_AGENT_ID: "ag_windows_smoke", AGENT_UI_INSTANCE_ID: "windows-smoke-instance", AGENT_UI_TREER_BIN: cli,
  AGENT_UI_MESSAGE_WATCH: "0", FAKE_ACP_CALLS: join(fixture, "acp-calls.jsonl"), FAKE_ACP_CHILD_PID_FILE: join(fixture, "provider-pids.json") });
for (const name of Object.keys(env)) if (/^NODE_/i.test(name)) delete env[name];
const installBase = join(machine, ".treer/launchers/agent");
function install(operation, version) {
  if (options.controller) return JSON.parse(execFileSync(options.controller, ["launcher", "--node", process.execPath, "--treer", cli, operation, ...(version ? [version] : [])], { env, encoding: "utf8", stdio: "pipe", timeout: 360000 }));
  const manifest = readFileSync(join(options.bundle, "bundle-manifest.json"));
  const request = { schema: "treer.launcher-install/v1", operation, bundle_dir: options.bundle, version: `sha256-${digest(manifest)}`,
    source_commit: JSON.parse(manifest).source_commit, root: machine, cwd: ".", agents: [], node: process.execPath, treer: cli, controller, allow_network: false, rollback_version: version };
  // Development-only JS mode is explicitly reported; not native embedded proof.
  protectDirectory(machine);
  return JSON.parse(execFileSync(process.execPath, [join(launcher, "scripts/install.mjs"), "--controller-request"], { input: JSON.stringify(request), env, encoding: "utf8", stdio: "pipe" }));
}
async function until(check, label, milliseconds = 90000) {
  const deadline = Date.now() + milliseconds;
  while (Date.now() < deadline) { if (await check()) return; await new Promise(done => setTimeout(done, 50)); }
  throw new Error(`Timed out: ${label}`);
}
function alive(pid) { try { process.kill(pid, 0); return true; } catch (error) { if (error.code === "ESRCH") return false; throw error; } }
let child, browser, base, logs = "", providerPids = [];
async function stop() {
  if (!child) return;
  stopProcessTree(child);
  await until(() => child.exitCode !== null || child.signalCode !== null, "fixture wrapper exit", 15000);
  await until(() => providerPids.every(pid => !alive(pid)), "provider/grandchild cleanup", 15000);
  child = undefined;
}
function aclCheck(directory) {
  if (!windows) return;
  const script = "$ErrorActionPreference='Stop'; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $acl=[IO.Directory]::GetAccessControl($env:FIXTURE_ACL_PATH); if(!$acl.AreAccessRulesProtected){throw 'inherited ACL'}; $rules=$acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]); foreach($rule in $rules){if($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -ne $sid -and $rule.IdentityReference.Value -ne 'S-1-5-18'){throw 'foreign principal'}}; if($rules.Count -lt 2){throw 'missing rules'}";
  execFileSync(join(process.env.SystemRoot, "System32/WindowsPowerShell/v1.0/powershell.exe"), ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")], { env: { ...env, FIXTURE_ACL_PATH: toNamespacedPath(directory) }, stdio: "pipe", timeout: 15000 });
}
async function start() {
  rmSync(join(fixture, "registration.json"), { force: true });
  const shim = join(fixture, "fake-acp.cmd"), entry = join(fixture, "node_modules/fake-acp/cli.mjs");
  mkdirSync(resolve(entry, ".."), { recursive: true });
  // Standard npm shim; never invokes cmd.exe. Keep fake fixture relative imports absent.
  writeFileSync(entry, readFileSync(join(launcher, "tests/fake-acp.mjs")));
  writeFileSync(shim, '@echo off\r\n"%dp0%\\node_modules\\fake-acp\\cli.mjs" %*\r\n');
  child = spawnProvider(process.execPath, [join(installBase, "run.mjs"), "--harness", "acp", "--acp-command", windows ? shim : entry, "--cwd", machine], { cwd: machine, env, stdio: "pipe" });
  child.stdout.on("data", data => logs += data); child.stderr.on("data", data => logs += data);
  await until(() => {
    if (child.exitCode !== null) throw new Error(`Fixture launcher exited: ${logs}`);
    return existsSync(join(fixture, "registration.json"));
  }, "manifest registration");
  const registration = JSON.parse(readFileSync(join(fixture, "registration.json")));
  base = `http://127.0.0.1:${registration.port}`;
  assert.deepEqual(registration.manifest.capabilities.slice().sort(), contract.capabilities.slice().sort());
  assert.equal(registration.manifest.ui_path, "/");
  providerPids = JSON.parse(readFileSync(env.FAKE_ACP_CHILD_PID_FILE));
}
const headers = { "x-treer-interface-source": "controller-api", "x-treer-agent-id": env.TREER_AGENT_ID, "x-treer-interface-instance": env.AGENT_UI_INSTANCE_ID };
try {
  const installed = install("install"); assert.equal(installed.portable, true); assert.equal(installed.profiles.length, 6);
  assert.equal(install("install").version, installed.version); assert.equal(install("update").version, installed.version);
  assert.equal(install("status").versions[0].compatibility.status, "compatible");
  assert.equal(install("rollback", installed.version).version, installed.version);
  const profile = JSON.parse(readFileSync(join(fixture, "Codex + UI.json")));
  assert.equal(profile.command, process.execPath); assert.equal(profile.args[0], join(installBase, "run.mjs"));
  if (windows) { assert.ok(join(installBase, installed.version, "runtime.mjs").length > 260); aclCheck(installBase); }
  writeFileSync(join(machine, "README.md"), "# Windows fixture\nLiteral <script> content\n");
  await start();
  const state = await (await localFetch(base + "/v1/state", { headers })).json();
  assert.equal(state.ready, true); assert.equal(state.thread.id, "sess-new");
  const response = await localFetch(base + "/v1/prompts", { method: "POST", headers: { ...headers, "content-type": "application/json" },
    body: JSON.stringify({ operation_id: "windows-smoke-1", text: "Native smoke literal $ & 中文", mode: "prompt", delivery: "queue" }) });
  assert.equal(response.status, 202, await response.text());
  await until(async () => JSON.stringify(await (await localFetch(base + "/v1/state", { headers })).json()).includes("Done now."), "ACP prompt response");
  assert.ok(JSON.stringify(await (await localFetch(base + "/v1/transcript", { headers })).json()).includes("Done now."));
  const stateDir = join(machine, ".treer/agents", env.TREER_AGENT_ID), stateFile = join(stateDir, "agent-ui.json");
  assert.equal(JSON.parse(readFileSync(stateFile)).threadId, "sess-new"); aclCheck(stateDir);
  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, extraHTTPHeaders: { ...headers, "x-treer-interface-source": "proxy-ui" } });
  const page = await context.newPage(), errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.goto(base + "/?theme=light"); await expect(page.locator("#timeline")).toContainText("Done now.");
  await page.getByRole("button", { name: "Browse files", exact: true }).click();
  await page.getByRole("button", { name: "README.md", exact: true }).click();
  await expect(page.locator("#file-content")).toContainText("Literal <script> content");
  await page.screenshot({ path: join(options.screenshots, "windows-agent-desktop.png") });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({ path: join(options.screenshots, "windows-agent-390px.png") });
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)); assert.deepEqual(errors, []);
  await browser.close(); browser = undefined;
  await stop(); await start();
  const calls = readFileSync(env.FAKE_ACP_CALLS, "utf8").trim().split("\n").map(JSON.parse);
  assert.equal(calls.filter(call => call.method === "session/new").length, 1);
  assert.ok(calls.some(call => call.method === "session/load" && call.params.sessionId === "sess-new"));
  assert.equal(JSON.parse(readFileSync(stateFile)).threadId, "sess-new");
  await stop();
  const report = { platform: process.platform, embedded_controller: Boolean(options.controller), portable_install: true, profiles: 6,
    registration: "exact manifest/CLI contract", prompt: "passed", transcript: "passed", session_save_resume: "passed", process_tree_cleanup: "passed",
    windows_acl_and_long_paths: windows ? "passed" : "not tested on macOS/Linux", provider: windows ? "npm .cmd via absolute Node" : "explicit JS via absolute Node",
    runtime_path: "empty", real_agents_touched: 0, real_credentials: false };
  writeFileSync(join(options.screenshots, "windows-smoke.json"), JSON.stringify(report, null, 2)); console.log(JSON.stringify(report));
} finally {
  await browser?.close();
  try { await stop(); } finally { rmSync(fixture, { recursive: true, force: true }); }
}
