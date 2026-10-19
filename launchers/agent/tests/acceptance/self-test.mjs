// Self-test of console-embed-acceptance.mjs against a LOCAL STAND-IN (not Treer's Proxy or Console):
//   a "console" origin (127.0.0.1) whose agent page frames the launcher through an "agent origin" (localhost) that
//   authenticates with a session cookie, adds frame-ancestors for the console, and forwards to the real launcher
//   (fake Codex). It proves the acceptance script's mechanics: it passes the good stand-in and FAILS each broken one.
//   It does NOT model Treer's nonce/bootstrap handoff; that is what the canary run is for.
// Usage: node tests/acceptance/self-test.mjs   (Chromium and Firefox; WebKit blocks third-party cookies, which this
//   stand-in's cookie model relies on, so it is left to the canary run)
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer, request as httpRequest } from "node:http";
import { connect } from "node:net";
import { randomUUID } from "node:crypto";
import { mkdtemp, mkdir, writeFile, readFile, rm, realpath, symlink, copyFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { hermeticEnv, localFetch } from "../hermetic.mjs";
import { fakePrivateState } from "../fake-private-state.mjs";

import { app, agentId, workspacePath, cleanup, startLauncher, startStandIn, writeState } from "./standin.mjs";
const script = join(import.meta.dirname, "console-embed-acceptance.mjs");

// SELF_TEST_PW_NODE_MODULES=/dir/with/@playwright lets the same script bytes run with another Playwright install, e.g. one
// whose Firefox/WebKit builds are present locally (the script resolves @playwright/test from its own location).
async function scriptPath() {
  if (!process.env.SELF_TEST_PW_NODE_MODULES) return script;
  const directory = await mkdtemp(join(tmpdir(), "accept-script-")); cleanup.push(() => rm(directory, { recursive: true, force: true }));
  await symlink(process.env.SELF_TEST_PW_NODE_MODULES, join(directory, "node_modules"));
  const copy = join(directory, "console-embed-acceptance.mjs"); await copyFile(script, copy);
  return copy;
}

// Async on purpose: the stand-in servers live in this process, so a blocking spawnSync would starve them.
function execute(file, options) {
  return new Promise((done) => {
    const child = spawn(process.execPath, [file], { cwd: options.cwd, env: options.env, stdio: "pipe" });
    let stdout = "", stderr = "";
    const timer = options.timeout ? setTimeout(() => child.kill("SIGKILL"), options.timeout) : undefined;
    child.stdout.on("data", (c) => { stdout += c; }); child.stderr.on("data", (c) => { stderr += c; });
    child.on("exit", (status) => { clearTimeout(timer); done({ status, stdout, stderr }); });
  });
}

async function runScript(path, consolePort, statePath, extra = {}) {
  const reportFile = join(tmpdir(), `accept-report-${randomUUID()}.json`);
  const result = await execute(path, { cwd: app, timeout: 900_000, env: { ...process.env, ACCEPT_CONSOLE_URL: `http://127.0.0.1:${consolePort}`, ACCEPT_WORKSPACE_PATH: workspacePath, ACCEPT_AGENT_ID: agentId, ACCEPT_STORAGE_STATE: statePath, ACCEPT_ENGINES: process.env.SELF_TEST_ENGINES || "chromium,firefox", ACCEPT_ALLOWED_HOST_SUFFIXES: "localhost,127.0.0.1", ACCEPT_ALLOW_HTTP: "1", ACCEPT_ISOLATED_HOST_REGEX: "^localhost$", ACCEPT_TIMEOUT_MS: "15000", ACCEPT_REPORT_FILE: reportFile, ...extra } });
  return { ...result, reportFile };
}

try {
  const runPath = await scriptPath();
  const launcherPort = await startLauncher();
  const stateDir = await mkdtemp(join(tmpdir(), "accept-state-")); cleanup.push(() => rm(stateDir, { recursive: true, force: true }));
  const statePath = join(stateDir, "state.json"); await writeState(statePath);
  const expectations = [
    ["good", 0, null],
    ["no-frame-ancestors", 1, "a foreign embedder cannot frame"],
    ["anonymous-open", 1, "without a session shows no conversation"],
    ["blank-embed", 1, "embedded view reaches the Agent UI"],
    ["shared-origin", 1, "not an isolated agent origin"],
    ["wide-ancestors", 1, "not limited to the Console origin"],
    ["anonymous-silent", 1, "no refusal evidence"],
    // The same good stand-in under a stricter host rule: a frame host that is not an ag-* host must not pass as isolated.
    ["good", 1, "not an isolated agent origin", { ACCEPT_ISOLATED_HOST_REGEX: "^ag-" }],
    // An edge beacon in the Console page: blocked by the run; it fails the run unless the operator names it as ignored.
    ["beacon", 0, null, { ACCEPT_IGNORED_BLOCKED_HOSTS: "static.cloudflareinsights.com" }],
    ["beacon", 1, "no console errors", {}],
    // The script's own cross-origin read may log a SecurityError: ignored inside its probe window (and counted), but the same
    // text thrown by the page at any other time must fail the run (security 5199).
    ["probe-logs", 0, null, {}],
    // The locale pin is really applied: an explicit locale shows up on the requests (the UI falls back to English for fr).
    ["good", 0, null, { ACCEPT_LOCALE: "fr-FR" }],
    // (The Console page is closed shortly after the probe step, so a message that arrives after the window closes cannot be
    // staged reliably here; the window closing is a one-line reset in the script and is not covered by a case.)
    ["probe-text-outside", 1, "no console errors", {}],
    // A refused top-level page must be reported with its status, and labelled standalone, not blamed on the embedded view.
    ["standalone-403", 1, "the standalone page answered HTTP 403", {}],
  ];
  for (const [mode, code, expectedFailure, extra = {}] of expectations) {
    const seenLanguages = [];
    const { consolePort } = await startStandIn(launcherPort, mode, seenLanguages);
    const result = await runScript(runPath, consolePort, statePath, extra);
    const output = result.stdout + result.stderr;
    assert.equal(result.status, code, `${mode}: exit ${result.status}\n${output}`.slice(0, 4000));
    if (expectedFailure) assert.match(output, new RegExp(`FAIL[^\\n]*|FAIL ${expectedFailure}|${expectedFailure}`), `${mode}: names the failing step\n${output}`);
    if (expectedFailure) assert.ok(output.includes(expectedFailure), `${mode}: expected failing step "${expectedFailure}"\n${output}`);
    const report = JSON.parse(await readFile(result.reportFile, "utf8"));
    assert.equal(report.passed, code === 0, mode);
    if (mode === "shared-origin" || extra.ACCEPT_ISOLATED_HOST_REGEX) {
      // A step that did not run must say so: never PASS, and the reason is the real one (not a flag that was not set).
      for (const engine of report.results) for (const title of ["the Console document is not served", "standalone Agent page", "negative: a foreign embedder", "negative: the Agent origin without a session"]) {
        const entry = engine.steps.find((candidate) => candidate.step.includes(title));
        assert.equal(entry?.status, "skipped", `${mode}: "${title}" must be skipped, was ${entry?.status}`);
        assert.equal(entry.detail, "not run: the embedded view is not isolated");
      }
    }
    if (mode === "good" && code === 0 && extra.ACCEPT_LOCALE) assert.ok(seenLanguages.length > 0 && seenLanguages.every((value) => value.startsWith(extra.ACCEPT_LOCALE)), `accept-language was ${JSON.stringify([...new Set(seenLanguages)])}`);
    else if (mode === "good" && code === 0) {
      // Every context is pinned to en-US, whatever the machine's own locale is (WebKit uses the system locale otherwise).
      assert.ok(seenLanguages.length > 0 && seenLanguages.every((value) => /^en-US/i.test(value)), `accept-language was ${JSON.stringify([...new Set(seenLanguages)])}`);
    }
    if (mode === "good" && code === 0) assert.ok(report.results.every((engine) => engine.documents.some((entry) => entry.label === "standalone")), "the standalone page's documents are labelled standalone");
    if (mode === "standalone-403") for (const engine of report.results) {
      assert.ok(engine.documents.some((entry) => entry.label === "standalone" && entry.status === 403), "the 403 belongs to the standalone page");
      assert.ok(!engine.documents.some((entry) => entry.label === "embedded" && entry.status >= 400), "the embedded view is not blamed");
      assert.equal(engine.steps.find((candidate) => candidate.step.includes("embedded view reaches")).status, "pass");
    }
    if (mode === "probe-logs") assert.ok(report.results.every((engine) => engine.ignored_own_probe_messages >= 1), "the in-window messages are counted, not hidden");
    if (mode === "probe-text-outside") for (const engine of report.results) {
      assert.equal(engine.ignored_own_probe_messages, 0, "outside the probe window nothing is ignored");
      assert.ok(JSON.stringify(engine.steps).includes("Blocked a frame"), "the message is reported as a finding");
    }
    if (mode === "beacon" && code === 0) assert.ok(report.results.every((engine) => engine.ignored_blocked_requests["static.cloudflareinsights.com"] >= 1), "beacon is counted, not hidden");
    assert.match(report.script_sha256, /^[0-9a-f]{64}$/);
    const text = JSON.stringify(report);
    assert.ok(!/sess=ok|"value":"ok"|set-cookie|iec_v1_/i.test(text), `${mode}: report must hold no cookie or token data`);
    await rm(result.reportFile, { force: true });
    console.log(`ok ${mode}: exit ${result.status}${expectedFailure ? ` (failed as intended at "${expectedFailure}")` : ""}`);
  }
  // Scope guards are refused before any browser starts: no allow-list, a bare registrable domain, a Console outside the list.
  const { consolePort: scopePort } = await startStandIn(launcherPort, "good");
  for (const [label, extra] of [["no allow-list", { ACCEPT_ALLOWED_HOST_SUFFIXES: "" }], ["bare domain", { ACCEPT_ALLOWED_HOST_SUFFIXES: "example.com" }], ["console not covered", { ACCEPT_ALLOWED_HOST_SUFFIXES: "app.canary.example" }]]) {
    const refusedScope = await runScript(runPath, scopePort, statePath, extra);
    assert.equal(refusedScope.status, 2, `${label}: ${refusedScope.stdout}${refusedScope.stderr}`.slice(0, 2000));
  }
  console.log("ok scope guards refused (exit 2): no allow-list, bare domain, Console not covered");
  // Missing inputs are refused before anything is launched.
  const refused = await execute(script, { cwd: app, env: { PATH: process.env.PATH } });
  assert.equal(refused.status, 2);
  console.log("ok missing inputs refused (exit 2)");
} finally {
  for (const task of cleanup.reverse()) await task().catch(() => {});
}
