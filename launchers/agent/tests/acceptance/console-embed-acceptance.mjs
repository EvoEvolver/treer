#!/usr/bin/env node
// Real-browser acceptance of the Console's embedded Agent interface and the standalone Agent page against a REAL
// launcher-backed agent (#98 team rule: no Console/browser-path change ships before this passes).
//
// It does not start or change anything: it drives the given Console in real Chromium, Firefox and WebKit using an
// existing disposable agent (fake harness, no real credentials) and reports a pass/fail per step.
//
// Inputs (environment only; nothing is read from argv so no secret lands in a process list):
//   ACCEPT_CONSOLE_URL      Console origin, e.g. https://console.example (no path)
//   ACCEPT_WORKSPACE_PATH   Console workspace path, e.g. /orgs/<org>/workspaces/<workspace>
//   ACCEPT_AGENT_ID         the disposable launcher agent's id (a workload id)
//   ACCEPT_STORAGE_STATE    path of a Playwright storage-state file for the test member (never printed, never copied)
//   ACCEPT_ENGINES          optional, comma list of chromium,firefox,webkit (default: all three)
//   ACCEPT_REPORT_FILE      optional, where to write the JSON report (default: ./console-embed-acceptance-report.json)
//   ACCEPT_ALLOWED_HOST_SUFFIXES  REQUIRED, comma list of canary hosts or canary-only suffixes the run may contact. Entries
//                           with fewer than three labels (a bare registrable domain) are refused, and the Console host must be
//                           covered. Any other request is aborted and fails the run.
//   ACCEPT_IGNORED_BLOCKED_HOSTS  optional, comma list of hosts whose (aborted, never contacted) requests are counted instead of
//                           failing the run, e.g. an edge analytics beacon injected into the Console
//   ACCEPT_ISOLATED_HOST_REGEX    optional, default ^ag- (the agent origin host form); ACCEPT_ALLOW_HTTP=1 only for a local stand-in
//   ACCEPT_TIMEOUT_MS       optional per-step wait, default 30000
//   ACCEPT_LOCALE           optional browser locale for every context, default en-US (the selectors expect the English UI)
//   ACCEPT_EXPECT_COMPATIBILITY  set to 1 only to accept a non-isolated embedded view (Console or Proxy origin); by default the
//                           embedded view MUST be served from the agent's own ag-* origin over https
//
// Exit code 0 only if every engine passes every step. The report records, per step, pass/fail plus status codes and a
// fixed allow-list of response headers of the documents it loaded. It never records cookies, storage state, request
// headers, query strings, fragments, or message bodies; console text is redacted for token-looking strings.
//
// No network except the Console: requests to any other host are aborted and counted as a failure.
// No traces, videos, HARs or screenshots are written.
import { createHash, randomUUID } from "node:crypto";
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { chromium, firefox, webkit, expect } from "@playwright/test";

const selfHash = createHash("sha256").update(readFileSync(new URL(import.meta.url))).digest("hex");
const env = process.env;
const required = ["ACCEPT_CONSOLE_URL", "ACCEPT_WORKSPACE_PATH", "ACCEPT_AGENT_ID", "ACCEPT_STORAGE_STATE"];
for (const name of required) if (!env[name]) { console.error(`missing ${name}`); process.exit(2); }
const consoleOrigin = new URL(env.ACCEPT_CONSOLE_URL).origin;
if (!/^\/orgs\/[^/]+\/workspaces\/[^/]+$/.test(env.ACCEPT_WORKSPACE_PATH)) { console.error("ACCEPT_WORKSPACE_PATH must look like /orgs/<org>/workspaces/<workspace>"); process.exit(2); }
if (!/^[a-zA-Z0-9_-]{1,128}$/.test(env.ACCEPT_AGENT_ID)) { console.error("ACCEPT_AGENT_ID is not a valid workload id"); process.exit(2); }
if (!existsSync(env.ACCEPT_STORAGE_STATE)) { console.error("ACCEPT_STORAGE_STATE file not found"); process.exit(2); }
const timeout = Number(env.ACCEPT_TIMEOUT_MS || 30000);
const reportFile = env.ACCEPT_REPORT_FILE || "console-embed-acceptance-report.json";
const engines = { chromium, firefox, webkit };
const wanted = (env.ACCEPT_ENGINES || "chromium,firefox,webkit").split(",").map((name) => name.trim()).filter(Boolean);
for (const name of wanted) if (!engines[name]) { console.error(`unknown engine ${name}`); process.exit(2); }
// Scope: the run may touch canary only. The operator must name the hosts; nothing is guessed from the Console host.
if (!env.ACCEPT_ALLOWED_HOST_SUFFIXES) { console.error("ACCEPT_ALLOWED_HOST_SUFFIXES is required (comma list of exact canary hosts or canary-only suffixes, e.g. app.canary.example,proxy.canary.example,canary.apps.example)"); process.exit(2); }
const consoleHost = new URL(consoleOrigin).hostname;
const isIp = (value) => /^\d+\.\d+\.\d+\.\d+$/.test(value) || value === "localhost";
const suffixes = env.ACCEPT_ALLOWED_HOST_SUFFIXES.split(",").map((value) => value.trim().toLowerCase()).filter(Boolean);
// A bare registrable domain (two labels, e.g. example.com) would admit production hosts; refuse it.
for (const suffix of suffixes) if (!isIp(suffix) && suffix.split(".").length < 3) { console.error(`allowed host entry "${suffix}" is too broad; name canary hosts (at least three labels)`); process.exit(2); }
const hostAllowed = (host) => suffixes.some((suffix) => host === suffix || host.endsWith(`.${suffix}`));
if (!hostAllowed(consoleHost)) { console.error("the Console host is not covered by ACCEPT_ALLOWED_HOST_SUFFIXES"); process.exit(2); }
// Hosts whose requests the run aborts and counts instead of failing on (e.g. an edge analytics beacon injected into the Console).
const ignoredBlocked = (env.ACCEPT_IGNORED_BLOCKED_HOSTS || "").split(",").map((value) => value.trim().toLowerCase()).filter(Boolean);
// The isolated frame host: ag-<id> under an allowed suffix over https (override only for a local stand-in).
const isolatedHost = new RegExp(env.ACCEPT_ISOLATED_HOST_REGEX || "^ag-[a-z0-9]+\\.");
// The selectors read English UI text, and some engines (WebKit) otherwise take the system locale: pin it.
const locale = env.ACCEPT_LOCALE || "en-US";
const requireHttps = env.ACCEPT_ALLOW_HTTP !== "1";
const agentUrl = consoleOrigin + env.ACCEPT_WORKSPACE_PATH + "/workloads/" + encodeURIComponent(env.ACCEPT_AGENT_ID);

const KEEP_HEADERS = ["content-security-policy", "x-frame-options", "cross-origin-opener-policy", "cross-origin-embedder-policy", "cross-origin-resource-policy", "referrer-policy", "x-content-type-options", "permissions-policy", "cache-control", "content-type"];
// URL without query or fragment; token-looking runs are masked so a secret can never reach the report or the log.
const clean = (value) => String(value).replace(/[?#].*$/, "").replace(/iec_v\d+_[0-9a-f]+/g, "iec_***").replace(/[A-Za-z0-9_-]{32,}/g, (run) => run.length > 40 && !/^[a-z-]+$/.test(run) ? "***" : run).slice(0, 300);
// isVisible() ignores timeouts and can hang on a blocked or cross-origin frame in some engines; waitFor honours them.
const appears = (locator, ms) => locator.waitFor({ state: "visible", timeout: ms }).then(() => true, () => false);
// A step that did not run reports "skipped", never "pass"; the reason names the real cause.
const skipped = (isolatedView) => ({ __skipped: env.ACCEPT_EXPECT_COMPATIBILITY === "1" ? "skipped (non-isolated view accepted by ACCEPT_EXPECT_COMPATIBILITY)" : "not run: the embedded view is not isolated" });
const origin = (value) => { try { return new URL(value).origin; } catch { return ""; } };

async function runEngine(name) {
  const steps = [], documents = [], problems = [], aborted = { count: 0 }, ignoredBlockedCount = {}, ownProbeMessages = { count: 0 }, probeWindow = { page: null, until: 0 };
  const step = async (title, fn) => {
    if (env.ACCEPT_TRACE === "1") console.error(`[${name}] > ${title}`);
    // No step may hang the run: a stuck browser call becomes a failed step after a hard deadline.
    const deadline = new Promise((_, reject) => setTimeout(() => reject(new Error(`step exceeded its ${timeout * 4} ms deadline`)), timeout * 4).unref());
    try {
      const detail = await Promise.race([fn(), deadline]);
      if (detail && typeof detail === "object" && "__skipped" in detail) steps.push({ step: title, status: "skipped", detail: detail.__skipped });
      else steps.push({ step: title, status: "pass", ...(detail ? { detail } : {}) });
    }
    catch (error) { steps.push({ step: title, status: "fail", detail: clean(error instanceof Error ? error.message.split("\n")[0] : String(error)) }); return false; }
    return true;
  };
  const browser = await engines[name].launch();
  const watch = (context, label) => {
    context.on("page", (page) => hook(page, label));
    for (const page of context.pages()) hook(page, label);
  };
  // Pages are labelled lazily so the standalone page is not reported as "embedded".
  const pageLabels = new WeakMap();
  const hook = (page, defaultLabel) => {
    const labelOf = () => pageLabels.get(page) ?? defaultLabel;
    // This script's own cross-origin read of the frame logs a "Blocked a frame ..." error in some engines; that is the probe, not a finding.
    // Narrow on purpose (security 5199): only that text, only on the Console page, only while the probe step runs
    // (plus a short grace for late events). The same text anywhere else is a real finding.
    const ownProbe = (text) => page === probeWindow.page && Date.now() <= probeWindow.until && /Blocked a frame with origin|accessing a frame with origin/.test(text);
    page.on("pageerror", (error) => { if (ownProbe(error.message)) { ownProbeMessages.count++; return; } problems.push(`${labelOf()} pageerror: ${clean(error.message)}`); });
    page.on("console", (message) => {
      // A console error that is about a host the run deliberately blocks and ignores is not a finding.
      if (ignoredBlocked.some((host) => message.text().includes(host) || (message.location().url || "").includes(host))) return;
      if (message.type() === "error" && ownProbe(message.text())) { ownProbeMessages.count++; return; }
      if (message.type() === "error") problems.push(`${labelOf()} console: ${clean(message.text())} @ ${clean(message.location().url || "")}`); });
    page.on("requestfailed", (request) => {
      const reason = request.failure()?.errorText || "";
      // An app cancelling its own fetch (AbortController, navigation) is not a failure; anything else is. Counted, not hidden.
      if (ignoredBlocked.includes(new URL(request.url()).hostname)) return;
      if (/ABORTED|cancel/i.test(reason) && ["fetch", "xhr"].includes(request.resourceType())) { aborted.count++; return; }
      problems.push(`${labelOf()} request failed: ${request.method()} ${clean(request.url())} (${clean(reason)})`);
    });
    page.on("response", (response) => {
      const status = response.status();
      const type = response.request().resourceType();
      if (type === "document") {
        const headers = response.headers();
        documents.push({ label: labelOf(), url: clean(response.url()), status, headers: Object.fromEntries(KEEP_HEADERS.filter((key) => key in headers).map((key) => [key, headers[key].slice(0, 400)])) });
      }
      if (status >= 400) problems.push(`${labelOf()} HTTP ${status} ${response.request().method()} ${clean(response.url())}`);
    });
  };
  const guard = async (context) => context.route("**/*", (route) => {
    const url = new URL(route.request().url());
    if (url.protocol === "about:" || url.protocol === "data:" || url.protocol === "blob:") return route.continue();
    if (hostAllowed(url.hostname)) return route.continue();
    if (ignoredBlocked.includes(url.hostname)) ignoredBlockedCount[url.hostname] = (ignoredBlockedCount[url.hostname] || 0) + 1;
    else problems.push(`blocked request outside the allowed hosts: ${clean(route.request().url())}`);
    return route.abort();
  });
  let embeddedOrigin = "", isolated = false;
  const nonce = `acceptance ${randomUUID().slice(0, 8)}`;
  try {
    // 1) Embedded view with the test member's session.
    const member = await browser.newContext({ locale, storageState: env.ACCEPT_STORAGE_STATE, viewport: { width: 1360, height: 900 } });
    await guard(member); watch(member, "embedded");
    const page = await member.newPage();
    const frame = page.frameLocator('iframe[title$=" interface"]');
    await step("Console opens the agent tab", async () => {
      await page.goto(agentUrl, { waitUntil: "domcontentloaded", timeout });
      await expect(page.locator('iframe[title$=" interface"]')).toBeVisible({ timeout });
    });
    await step("embedded view reaches the Agent UI (from an isolated agent origin)", async () => {
      // Decide the origin first so a shared-origin view fails with its real reason rather than a timeout.
      const src = await page.locator('iframe[title$=" interface"]').getAttribute("src");
      embeddedOrigin = origin(new URL(src || "", consoleOrigin).href);
      const frameHost = new URL(embeddedOrigin).hostname;
      isolated = embeddedOrigin !== consoleOrigin && isolatedHost.test(frameHost) && hostAllowed(frameHost) && (!requireHttps || embeddedOrigin.startsWith("https://"));
      if (!isolated && env.ACCEPT_EXPECT_COMPATIBILITY !== "1") throw new Error(`embedded view is served from ${clean(embeddedOrigin)}, not an isolated agent origin (ag-* host under an allowed suffix${requireHttps ? ", https" : ""})`);
      await expect(frame.getByRole("textbox", { name: /Message/ })).toBeVisible({ timeout });
      // The handshake leaves a visible notice only for a failure or the compatibility choice.
      const notice = await page.getByRole("alert").filter({ hasText: /compatibility|unavailable|did not load|Cannot/i }).count();
      return { frame_origin: embeddedOrigin, framed_on: isolated ? "agent origin" : "non-isolated origin (accepted by ACCEPT_EXPECT_COMPATIBILITY)", notice_visible: notice > 0 };
    });
    await step("embedded: first paint shows conversation chrome and no error banner", async () => {
      await expect(frame.locator("header.top")).toBeVisible({ timeout });
      await expect(frame.getByRole("alert")).toHaveCount(0);
    });
    await step("embedded: sending a message reaches the launcher and the composer clears", async () => {
      // A fixture harness may never echo the turn back, so the transcript is not asserted; the launcher accepting the
      // prompt (HTTP 200 on /api/prompt through the embed path) and the UI clearing its composer is.
      const box = frame.getByRole("textbox", { name: /Message/ });
      await box.fill(nonce);
      await expect(frame.getByRole("button", { name: "Send message", exact: true })).toBeEnabled({ timeout });
      const accepted = page.waitForResponse((response) => response.request().method() === "POST" && /\/api\/prompt$/.test(new URL(response.url()).pathname), { timeout });
      await box.press("Enter");
      const response = await accepted;
      if (response.status() !== 200) throw new Error(`the launcher answered the prompt with HTTP ${response.status()}`);
      await expect(box).toHaveValue("", { timeout });
      const alerts = (await frame.getByRole("alert").allInnerTexts().catch(() => [])).map((text) => clean(text.slice(0, 200)));
      if (alerts.length) throw new Error(`the UI shows an alert after sending: ${alerts.join(" | ")}`);
    });
    await step("embedded: files panel opens and lists the working directory", async () => {
      await frame.getByRole("button", { name: "Browse files" }).click();
      const panel = frame.locator("#files");
      await expect(panel).toBeVisible({ timeout });
      await expect(panel.getByRole("tree", { name: "Files" })).toBeVisible({ timeout });
    });
    await step("embedded: the Console document is not served the Agent's origin session", async () => {
      // The Agent page must not be able to read the Console: the parent origin is cross-origin to a framed agent origin.
      if (!isolated) return skipped();
      // The read below makes some engines log a cross-origin SecurityError; open the narrow ignore window around it only.
      probeWindow.page = page; probeWindow.until = Infinity;
      let readable;
      try { readable = await page.evaluate(() => { try { return Boolean(document.querySelector("iframe")?.contentDocument); } catch { return false; } }); }
      finally { await page.waitForTimeout(1000); probeWindow.until = Date.now(); probeWindow.page = null; }
      if (readable) throw new Error("the Console can read the framed Agent document, so it is not isolated");
      return "cross-origin frame not readable";
    });

    // 2) Standalone page of the same agent (top-level), same member.
    const standaloneTarget = isolated ? embeddedOrigin + "/" : undefined;
    const standalone = await member.newPage();
    pageLabels.set(standalone, "standalone");
    await step("standalone Agent page loads for the member", async () => {
      if (!standaloneTarget) return skipped();
      // Keep the evidence: the final status and the redirect chain are what tells a refused page from a slow one.
      const response = await standalone.goto(standaloneTarget, { waitUntil: "domcontentloaded", timeout });
      let redirects = 0; for (let from = response?.request().redirectedFrom(); from; from = from.redirectedFrom()) redirects++;
      const evidence = { final_status: response?.status() ?? 0, redirects, final_host: new URL(standalone.url()).hostname, final_path: new URL(standalone.url()).pathname };
      if ((response?.status() ?? 0) >= 400) throw new Error(`the standalone page answered HTTP ${evidence.final_status} after ${redirects} redirect(s) at ${evidence.final_host}${evidence.final_path}`);
      if (!await appears(standalone.getByRole("textbox", { name: /Message/ }), timeout)) throw new Error(`no composer on the standalone page (HTTP ${evidence.final_status}, ${redirects} redirect(s), ${evidence.final_host}${evidence.final_path})`);
      await expect(standalone.locator("header.top")).toBeVisible({ timeout });
      return evidence;
    });
    await member.close();

    // 3) Negative: a foreign page must not be able to frame the Agent origin.
    // The threat is a malicious page in a browser that IS logged in, so this context carries the member session too.
    const foreign = await browser.newContext({ locale, storageState: env.ACCEPT_STORAGE_STATE, viewport: { width: 1000, height: 700 } });
    await guard(foreign);
    await step("negative: a foreign embedder cannot frame the Agent origin", async () => {
      if (!isolated) return skipped();
      // Header evidence first: not rendering could also be a network failure, so the policy itself must be on the response.
      const agentDocument = documents.find((entry) => entry.label === "embedded" && entry.url.startsWith(embeddedOrigin + "/") && entry.status < 400);
      const policy = agentDocument?.headers["content-security-policy"] || "";
      const directive = /(?:^|;)\s*frame-ancestors\s+([^;]*)/i.exec(policy);
      if (!directive) throw new Error("the Agent origin document carries no CSP frame-ancestors directive");
      const ancestors = directive[1].trim().split(/\s+/);
      if (!ancestors.includes(consoleOrigin) || ancestors.some((token) => token !== consoleOrigin && token !== "'self'")) throw new Error(`CSP frame-ancestors is not limited to the Console origin: ${clean(directive[1])}`);
      const foreignPage = await foreign.newPage();
      await foreignPage.setContent(`<!doctype html><iframe id="f" src="${embeddedOrigin}/" style="width:900px;height:600px"></iframe>`, { waitUntil: "commit", timeout });
      await foreignPage.waitForTimeout(8000);
      const framed = foreignPage.frameLocator("#f");
      const visible = await appears(framed.getByRole("textbox", { name: /Message/ }), 4000);
      if (visible) throw new Error("the Agent origin rendered inside a foreign page");
      return { refused: true, frame_ancestors: ancestors };
    });
    await foreign.close();

    // 4) Negative: no session, no Agent UI.
    const anonymous = await browser.newContext({ locale, viewport: { width: 1000, height: 700 } });
    await guard(anonymous);
    await step("negative: the Agent origin without a session shows no conversation", async () => {
      if (!isolated) return skipped();
      const anonymousPage = await anonymous.newPage();
      // A network failure must not look like a refusal: require a 401/403 or a redirect away (to the authorize step).
      const response = await anonymousPage.goto(embeddedOrigin + "/", { waitUntil: "domcontentloaded", timeout });
      let redirects = 0; for (let from = response?.request().redirectedFrom(); from; from = from.redirectedFrom()) redirects++;
      const status = response?.status() ?? 0;
      await anonymousPage.waitForTimeout(3000);
      const composer = await appears(anonymousPage.getByRole("textbox", { name: /Message/ }), 3000);
      if (composer) throw new Error("the Agent UI is reachable without a session");
      if (!(status === 401 || status === 403 || redirects > 0)) throw new Error(`no refusal evidence: final status ${status} with no redirect`);
      return { final_status: status, redirects, final_host: new URL(anonymousPage.url()).hostname };
    });
    await anonymous.close();
  } catch (error) {
    steps.push({ step: "run", status: "fail", detail: clean(error instanceof Error ? error.message.split("\n")[0] : String(error)) });
  } finally { await Promise.race([browser.close().catch(() => {}), new Promise((resolve) => setTimeout(resolve, 15000).unref())]); }
  // Negative checks by design provoke failed loads on the foreign/anonymous contexts; only the member's pages must be clean.
  const memberProblems = problems.filter((line) => line.startsWith("embedded") || line.startsWith("standalone"));
  const blocked = problems.filter((line) => line.startsWith("blocked request"));
  if (!memberProblems.length && !blocked.length) steps.push({ step: "no console errors, failed requests or HTTP errors on the member's pages", status: "pass" });
  else steps.push({ step: "no console errors, failed requests or HTTP errors on the member's pages", status: "fail", detail: [...memberProblems, ...blocked].slice(0, 20) });
  return { engine: name, passed: steps.every((entry) => entry.status === "pass" || (entry.status === "skipped" && env.ACCEPT_EXPECT_COMPATIBILITY === "1")), steps, ignored_aborted_fetches: aborted.count, ignored_blocked_requests: ignoredBlockedCount, ignored_own_probe_messages: ownProbeMessages.count, documents };
}

const results = [];
for (const name of wanted) results.push(await runEngine(name));
const report = { script_sha256: selfHash, console_origin: consoleOrigin, workspace_path: env.ACCEPT_WORKSPACE_PATH, agent_id: env.ACCEPT_AGENT_ID, passed: results.every((entry) => entry.passed), results };
writeFileSync(reportFile, JSON.stringify(report, null, 2) + "\n", { mode: 0o600 });
console.log(`script sha256 ${selfHash}`);
for (const result of results) {
  console.log(`${result.passed ? "PASS" : "FAIL"} ${result.engine}`);
  for (const entry of result.steps) console.log(`  ${entry.status.toUpperCase()} ${entry.step}${entry.status === "fail" ? `: ${JSON.stringify(entry.detail)}` : ""}`);
}
console.log(report.passed ? "ACCEPTANCE PASSED" : "ACCEPTANCE FAILED");
process.exit(report.passed ? 0 : 1);
