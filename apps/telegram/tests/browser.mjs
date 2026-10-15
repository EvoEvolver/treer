import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import { createRequire } from 'node:module';
import { mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.TELEGRAM_PLAYWRIGHT_MODULE || 'playwright');
const SECRET = '123456:abcdefghijklmnopqrstuvwxyz0123';
const screenshots = process.env.TELEGRAM_SCREENSHOT_DIR || '/tmp/treer-telegram-settings-screenshots';
const clean = () => Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(TREER_|AGENT_UI_|CODEX_UI_)/.test(k) && !/^(http_proxy|https_proxy|all_proxy|no_proxy)$/i.test(k)));

async function start() {
  const fixture = spawn(process.env.PYTHON || 'python3', [fileURLToPath(new URL('./browser_fixture.py', import.meta.url))], { env: clean(), stdio: ['ignore', 'pipe', 'pipe'] });
  let stderr = ''; fixture.stderr.on('data', c => stderr += c);
  const lines = createInterface({ input: fixture.stdout });
  const [line] = await once(lines, 'line');
  return { url: JSON.parse(line).url, fixture, lines, stderr: () => stderr };
}

for (const width of [1280, 390]) test(`settings page: token write-only, pairing code shown once, POC/users/pause, XSS-safe, ${width}px`, async () => {
  const { url, fixture, lines, stderr } = await start();
  const browser = await chromium.launch({ env: clean() });
  try {
    const context = await browser.newContext({ viewport: { width, height: 900 } });
    await context.setExtraHTTPHeaders({ 'X-Treer-Identity-Token': 'admin' });
    const page = await context.newPage();
    page.setDefaultTimeout(8000);
    const errors = []; page.on('pageerror', e => errors.push(e.message)); page.on('dialog', d => d.accept());
    const responses = []; page.on('response', async r => { try { responses.push(await r.text()); } catch {} });
    await page.goto(url);
    await page.getByRole('heading', { name: 'Telegram bridge', exact: true }).waitFor();
    await page.getByText('Signed in as Ada · admin').waitFor();
    assert.match(await page.locator('#bridge-state').innerText(), /Offline/);
    await mkdir(screenshots, { recursive: true });
    await page.screenshot({ path: `${screenshots}/settings-initial-${width}.png`, fullPage: true });

    // A bad token is refused by Telegram; the password field is emptied either way.
    const tokenInput = page.getByLabel('Token from @BotFather');
    assert.equal(await tokenInput.getAttribute('type'), 'password');
    assert.equal(await tokenInput.getAttribute('autocomplete'), 'new-password');
    await tokenInput.fill('654321:abcdefghijklmnopqrstREJECTED000000');
    await page.getByRole('button', { name: 'Save token' }).click();
    await page.getByRole('alert').filter({ hasText: 'Telegram did not accept that token' }).waitFor();
    assert.equal(await tokenInput.inputValue(), '', 'the secret is cleared the moment it is submitted');
    await tokenInput.fill(SECRET);
    await page.getByRole('button', { name: 'Save token' }).click();
    await page.getByText('Token saved and verified with Telegram').waitFor();
    await page.getByText('@treer_test_bot', { exact: false }).first().waitFor();
    await page.getByText('…0123').waitFor();
    assert.equal(await page.getByLabel('Token from @BotFather').count(), 0, 'no input remains once a token is stored');
    const html = await page.content();
    assert(!html.includes(SECRET) && !html.includes('abcdefghijklmnopqrstuvwxyz0123'), 'the token is nowhere in the DOM');
    assert(!responses.some(r => r.includes('abcdefghijklmnopqrstuvwxyz0123')), 'no response ever carried the token');
    assert.equal(await page.evaluate(() => JSON.stringify([localStorage, sessionStorage]).includes('abcdefghij')), false);

    // Pairing: the code is displayed once, with a countdown, and is gone after a reload.
    await page.getByRole('button', { name: 'Generate pairing code' }).click();
    const code = (await page.locator('#pairing-code').innerText()).trim();
    assert.match(code, /^[A-Z0-9]{5}-[A-Z0-9]{5}$/);
    await page.getByText(/Shown once · expires in/).waitFor();
    await page.getByText(/\/pair CODE/).first().waitFor();
    await page.screenshot({ path: `${screenshots}/settings-pairing-${width}.png`, fullPage: true });
    await page.reload();
    await page.getByText(/A code is active until/).waitFor();
    assert.equal(await page.locator('#pairing-code').count(), 0);
    assert(!(await page.content()).includes(code), 'the code cannot be shown again');
    await page.getByRole('button', { name: 'Cancel code' }).click();
    await page.getByText('Pairing code cancelled').waitFor();

    // POC, users, pause and the change log; hostile labels are text.
    await page.getByLabel('Conversation (chat ID or CHAT:THREAD)').fill('-100123:5');
    await page.getByLabel('Point of contact for the new conversation').fill('/team/poc');
    await page.getByRole('button', { name: 'Bind', exact: true }).click();
    await page.getByText('Binding saved').waitFor();
    await page.getByLabel('Point of contact for -100123:5', { exact: true }).waitFor();
    await page.getByLabel('Telegram user ID').fill('4242');
    await page.getByLabel('Label for the new user').fill('<img src=x onerror=window.xss=1>');
    await page.getByRole('button', { name: 'Add or update' }).click();
    await page.getByText('User saved').waitFor();
    await page.getByRole('cell', { name: '<img src=x onerror=window.xss=1>' }).waitFor();
    assert.equal(await page.locator('img').count(), 0);
    assert.equal(await page.evaluate(() => globalThis.xss), undefined);
    await page.getByRole('button', { name: 'Pause', exact: true }).first().click();      // the binding row's button
    await page.getByText('Binding paused').waitFor();
    await page.getByRole('button', { name: 'Pause', exact: true }).last().click();       // the bridge-wide button
    await page.getByText('Paused', { exact: true }).first().waitFor();
    assert.equal(await page.locator('#bridge-state').innerText(), 'Paused');
    await page.getByRole('button', { name: 'Resume', exact: true }).last().click();
    await page.getByText('Resumed').waitFor();
    const log = await page.locator('.audit').innerText();
    for (const op of ['token.set', 'pairing.create', 'pairing.cancel', 'poc.set', 'users.set', 'bridge.pause']) assert.match(log, new RegExp(op));
    assert.match(log, /human:usr_admin/);
    assert(!log.includes('abcdefghijklmnopqrstuvwxyz0123'));
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'no horizontal overflow');
    await page.screenshot({ path: `${screenshots}/settings-full-${width}-light.png`, fullPage: true });
    await page.evaluate(() => { document.documentElement.dataset.theme = 'dark'; });
    await page.screenshot({ path: `${screenshots}/settings-full-${width}-dark.png`, fullPage: true });

    // Clearing removes the token and says so.
    await page.getByRole('button', { name: 'Remove token' }).click();
    await page.getByText('Token removed').waitFor();
    await page.getByLabel('Token from @BotFather').waitFor();
    assert.deepEqual(errors, []);
    assert(!stderr().includes('abcdefghijklmnopqrstuvwxyz0123'));
  } finally { await browser.close(); fixture.kill(); lines.close(); await once(fixture, 'exit'); }
});

test('a workspace member without the admin role is refused, and sees no controls', async () => {
  const { url, fixture, lines } = await start();
  const browser = await chromium.launch({ env: clean() });
  try {
    const context = await browser.newContext({ viewport: { width: 390, height: 800 } });
    await context.setExtraHTTPHeaders({ 'X-Treer-Identity-Token': 'member' });
    const page = await context.newPage(); page.setDefaultTimeout(8000);
    await page.goto(url);
    await page.getByText(/Only workspace owners\/admins/).waitFor();
    assert.equal(await page.getByRole('button').count(), 0);
    const direct = await page.evaluate(async () => (await fetch(new URL('v1/token', document.baseURI), { method: 'PUT', headers: { 'Content-Type': 'application/json', 'X-Telegram-Settings': '1' }, body: JSON.stringify({ token: '123456:abcdefghijklmnopqrstuvwxyz0123' }) })).status);
    assert.equal(direct, 403, 'the API refuses what the page hides');
    const anon = await browser.newContext(); const anonPage = await anon.newPage();
    assert.equal((await anonPage.goto(url + 'v1/status')).status(), 401);
  } finally { await browser.close(); fixture.kill(); lines.close(); await once(fixture, 'exit'); }
});
