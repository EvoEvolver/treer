// Real-browser check of where the conversation opens and when it follows, against a fixture-harness launcher with a
// long transcript (prose, code blocks, command output, images that decode after first layout).
//   RUST_SPIKE_BIN=$CARGO_TARGET_DIR/debug/treer-launch node --test tests/browser-scroll.mjs
// Standalone and inside a Console-like frame (cross-site parent, bootstrap document first, a status bar removed after the
// frame loads, inactive tabs hidden with `hidden`), desktop and narrow, fast and slow history. All data is synthetic.
import assert from 'node:assert/strict';
import test from 'node:test';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { mkdir, mkdtemp, readFile, realpath, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { hermeticEnv, localFetch as fetch } from './hermetic.mjs';
import { fakePrivateState } from './fake-private-state.mjs';

const binary = process.env.RUST_SPIKE_BIN;
assert.ok(binary, 'RUST_SPIKE_BIN required');
const { chromium } = createRequire(resolve('package.json'))('@playwright/test');
const AGENT = 'ag_scroll', INSTANCE = 'scroll-instance', TURNS = 22;
const headers = { 'x-treer-interface-source': 'proxy-ui', 'x-treer-agent-id': AGENT, 'x-treer-interface-instance': INSTANCE };

async function until(read, accept, waitMs = 15000) {
  const end = Date.now() + waitMs;
  while (Date.now() < end) { try { const value = await read(); if (accept(value)) return value; } catch { /* retry */ } await new Promise((r) => setTimeout(r, 25)); }
  throw Error('browser scroll fixture deadline');
}

async function launcher(t) {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'scroll-'))), cwd = join(root, 'cwd'), state = join(root, 'private'), port = join(root, 'port');
  await mkdir(cwd, { mode: 0o700 }); await mkdir(state, { mode: 0o700 });
  const controller = await fakePrivateState(AGENT, 'fixture-only');
  const child = spawn(binary, [], { cwd, stdio: ['ignore', 'pipe', 'pipe'], env: hermeticEnv({
    HOME: root, CODEX_HOME: join(root, 'codex'), AGENT_UI_CWD: cwd, AGENT_UI_PUBLIC_DIR: resolve('public'), AGENT_UI_PORT_FILE: port, AGENT_UI_PORT: '0',
    TREER_AGENT_ID: AGENT, TREER_WORKLOAD_CREDENTIAL: 'fixture-only', TREER_AGENT_SERVER_URL: controller.url, TREER_AGENT_PRIVATE_STATE_DIR: state,
    AGENT_UI_INSTANCE_ID: INSTANCE, AGENT_UI_MESSAGE_WATCH: '0', RUST_SPIKE_REGISTER: '0', RUST_SPIKE_PROVIDER_LOG: join(root, 'provider.jsonl'),
    CODEX_BIN: resolve('tests/fake-codex.mjs'), FAKE_CODEX_LONG: String(TURNS), FAKE_CODEX_LONG_STEPS: '6', FAKE_CODEX_COMPLETE_TURNS: '1',
  }) });
  let logs = ''; child.stdout.on('data', (b) => logs += b); child.stderr.on('data', (b) => logs += b);
  t.after(async () => { if (child.exitCode === null && child.signalCode === null) { child.kill('SIGTERM'); await once(child, 'exit'); } await controller.close(); await rm(root, { recursive: true, force: true }); });
  let base;
  try {
    base = await until(async () => `http://127.0.0.1:${Number(await readFile(port, 'utf8'))}`, (v) => !v.endsWith('NaN'));
    await until(() => fetch(base + '/api/state').then((r) => r.json()), (s) => s.ready && s.thread.turns.length === TURNS);
  } catch (error) { throw Error(error.message + ' ' + logs); }
  const prompt = (text) => fetch(base + '/api/prompt', { method: 'POST', body: JSON.stringify({ prompt: text, mode: 'queue' }) });
  return { base, prompt };
}

// A Console stand-in on another site (localhost vs 127.0.0.1): bootstrap document first, then the UI, a status bar that
// disappears once framed, and a tab panel the test can hide like an inactive Console tab.
async function consolePage(t, base) {
  const server = createServer((_, res) => {
    res.setHeader('content-type', 'text/html');
    res.end(`<!doctype html><body style="margin:0;height:100vh;display:flex;flex-direction:column"><header style="height:56px;flex:none">Console</header>
<div style="position:relative;flex:1;min-height:0"><div id="panel" role="tabpanel" style="position:absolute;inset:0;overflow:hidden"><div style="display:flex;flex-direction:column;width:100%;height:100%;overflow:hidden">
<p id="notice" style="margin:0;padding:12px;border-bottom:1px solid">Checking…</p>
<iframe title="Agent interface" style="display:block;width:100%;flex:1;min-height:0;border:0" sandbox="allow-scripts allow-forms allow-same-origin allow-modals allow-downloads allow-popups"></iframe></div></div></div>
<script>const f=document.querySelector('iframe');let loads=0;f.onload=()=>{if(++loads===2)document.getElementById('notice').remove()};f.src=${JSON.stringify(base + '/.treer/embed')};setTimeout(()=>{f.src=${JSON.stringify(base + '/?lang=en')}},300);</script></body>`);
  }).listen(0);
  await once(server, 'listening');
  t.after(() => new Promise((r) => server.close(r)));
  return `http://localhost:${server.address().port}/`;
}

const position = (frame) => frame.evaluate(() => {
  const e = document.querySelector('#thread');
  return { top: e.scrollTop, max: e.scrollHeight - e.clientHeight, client: e.clientHeight, jump: Boolean(document.querySelector('.jump')), images: [...document.querySelectorAll('.chat img')].filter((i) => i.complete && i.naturalWidth > 0).length };
});
const atBottom = (p) => p.client > 0 && p.max > 4000 && p.max - p.top < 48 && !p.jump;
async function settledAtBottom(frame, what) {
  // Hold for a while: later layout growth (images, fonts) must not leave the view behind.
  const first = await until(() => position(frame), atBottom);
  assert.ok(first.images >= TURNS - 2, `${what}: transcript images decoded`);
  await new Promise((r) => setTimeout(r, 1200));
  const later = await position(frame);
  assert.ok(atBottom(later), `${what}: still at the latest message ${JSON.stringify(later)}`);
}

async function open(browser, target, { width, slow }) {
  const context = await browser.newContext({ viewport: { width, height: 850 }, extraHTTPHeaders: headers });
  const page = await context.newPage();
  if (slow) {
    // History arrives late over both the HTTP read and the socket.
    await page.route(/\/api\/state/, async (route) => { await new Promise((r) => setTimeout(r, 1500)); await route.continue(); });
    await page.routeWebSocket(/./, (ws) => { const server = ws.connectToServer(); server.onMessage((m) => setTimeout(() => ws.send(m), 1500)); ws.onMessage((m) => server.send(m)); });
  }
  await page.goto(target);
  const frame = target.includes('localhost') ? await (await page.waitForSelector('iframe[title="Agent interface"]')).contentFrame() : page.mainFrame();
  await until(async () => (await frame.url()).includes('lang=en') && await frame.locator('.chat .turn').count() > 0, Boolean);
  return { context, page, frame };
}

test('the conversation opens at the latest message and keeps following until the reader scrolls up', { timeout: 240000 }, async (t) => {
  const agent = await launcher(t);
  const consoleUrl = await consolePage(t, agent.base);
  const browser = await chromium.launch({ env: hermeticEnv({ HOME: process.env.HOME }) }); t.after(() => browser.close());
  for (const width of [1280, 390]) for (const embedded of [false, true]) for (const slow of [false, true]) {
    const what = `${width}px ${embedded ? 'embedded' : 'standalone'}${slow ? ' slow history' : ''}`;
    const { context, page, frame } = await open(browser, embedded ? consoleUrl : agent.base + '/?lang=en', { width, slow });
    await settledAtBottom(frame, what);

    // The browser resetting the scroll position (a frame laid out while hidden, a placeholder swap) is not the reader.
    await frame.evaluate(() => { document.querySelector('#thread').scrollTop = 0; });
    await until(() => position(frame), atBottom, 3000).catch(() => assert.fail(`${what}: a browser reset to the top was taken for the reader scrolling up`));

    if (embedded && !slow) {
      // An inactive Console tab is hidden; new work arriving meanwhile is the latest message when the tab is shown again.
      await page.evaluate(() => { document.getElementById('panel').hidden = true; });
      const text = `while hidden ${width}`;
      assert.equal((await agent.prompt(text)).status, 200);
      await new Promise((r) => setTimeout(r, 1500));
      await page.evaluate(() => { document.getElementById('panel').hidden = false; });
      await until(() => frame.evaluate((value) => document.querySelector('.chat').textContent.includes(value), text), Boolean);
      await settledAtBottom(frame, `${what} after the tab was shown again`);
    }

    if (!slow) {
      // The reader's own scrolling stops following; new content then offers a jump instead of moving the view.
      const box = await (embedded ? page.locator('iframe') : page.locator('#thread')).boundingBox();
      await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      for (let n = 0; n < 6; n++) { await page.mouse.wheel(0, -800); await new Promise((r) => setTimeout(r, 50)); }
      const up = await until(() => position(frame), (p) => p.max - p.top > 2000 && p.jump);
      const text = `while reading ${width} ${embedded}`;
      assert.equal((await agent.prompt(text)).status, 200);
      await until(() => frame.evaluate(() => document.querySelector('.jump')?.dataset.new === 'true'), Boolean);
      const reading = await position(frame);
      assert.ok(Math.abs(reading.top - up.top) < 4, `${what}: the reader's place is kept ${up.top} -> ${reading.top}`);
      // A browser reset while the reader is away (well after their last input) puts them back where they were.
      await new Promise((r) => setTimeout(r, 1200));
      await frame.evaluate(() => { document.querySelector('#thread').scrollTop = 0; });
      await until(() => position(frame), (p) => Math.abs(p.top - up.top) < 4, 3000);
      await frame.locator('.jump').click();
      await until(() => position(frame), atBottom);
    }
    await context.close();
  }
});
