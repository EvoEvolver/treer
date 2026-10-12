import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer, request } from 'node:http';
import { createInterface } from 'node:readline';
import { createRequire } from 'node:module';
import { mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.ISSUES_PLAYWRIGHT_MODULE || 'playwright');
function hermeticEnv(extra = {}) {
  return {...Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^(TREER_|AGENT_UI_|CODEX_UI_|ISSUES_)/.test(key) && !/^(http_proxy|https_proxy|all_proxy|no_proxy)$/i.test(key) && key !== 'CODEX_BIN')), ...extra};
}
const screenshotDir = process.env.ISSUES_SCREENSHOT_DIR || '/tmp/treer-issues-screenshots';

for (const width of [1280, 390]) test(`real OAuth, issue workflow, XSS and tunnel at ${width}px`, async () => {
  const fixture = spawn(process.env.PYTHON || 'python3', [fileURLToPath(new URL('./fixture.py', import.meta.url))], {env:hermeticEnv(), stdio:['ignore','pipe','pipe']});
  let stderr = ''; fixture.stderr.on('data',chunk => stderr += chunk);
  const lines = createInterface({ input: fixture.stdout });
  const [line] = await once(lines, 'line'); const config = JSON.parse(line);
  const errors = [];
  const tunnel = createServer((req,res) => {
    if (!req.url.startsWith('/tunnel/')) {res.writeHead(404);res.end();return;}
    const target = new URL(req.url.slice('/tunnel/'.length), config.url);
    const upstream = request(target,{method:req.method,headers:req.headers}, reply => {res.writeHead(reply.statusCode,reply.headers);reply.pipe(res);});
    upstream.on('error',e=>{res.writeHead(502);res.end(e.message);});req.pipe(upstream);
  });
  await new Promise(resolve=>tunnel.listen(0,'127.0.0.1',resolve));
  const browser = await chromium.launch({env:hermeticEnv()});
  try {
    const page = await browser.newPage({viewport:{width,height:900}});
    page.setDefaultTimeout(8000);
    page.on('pageerror',e=>errors.push(e.message));
    await page.goto(config.url);
    await page.getByRole('link',{name:'Sign in with Treer'}).click();
    await page.getByRole('button',{name:'New issue',exact:true}).waitFor();
    await page.getByRole('button',{name:'New issue',exact:true}).click();
    const title='Build <img src=x onerror=alert(1)> collaboration';
    await page.getByLabel('Title',{exact:true}).fill(title);
    await page.getByLabel('Description (Markdown source, rendered as text)',{exact:true}).fill('# Plan\n<script>window.xss=1</script>\nInvestigate shared development workflow.');
    await page.getByLabel('Assignee',{exact:true}).last().selectOption('ag_a');
    await page.getByLabel('Labels (comma separated)',{exact:true}).fill('team, backend');
    await page.getByRole('button',{name:'Create issue',exact:true}).click();
    await page.getByRole('heading',{name:title,exact:true}).waitFor();
    await page.getByRole('button',{name:'Add child',exact:true}).click();
    await page.getByLabel('Title',{exact:true}).fill('Reproduce build failure');
    await page.getByRole('button',{name:'Create issue',exact:true}).click();
    await page.getByRole('heading',{name:'Reproduce build failure',exact:true}).waitFor();
    await page.locator('#detail').getByLabel('Status',{exact:true}).selectOption('done');
    await page.getByRole('button',{name:'Save status / assignee',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#detail select')?.value==='done');
    await page.locator('#detail').getByRole('link',{name:'#1',exact:true}).click();
    await page.getByRole('heading',{name:/Children · 1\/1/}).waitFor();
    await page.getByLabel('Comment (text / Markdown source)',{exact:true}).fill('<svg onload=alert(1)> Ready for review.');
    await page.getByRole('button',{name:'Post comment',exact:true}).click();
    await page.locator('.comment-body').getByText('<svg onload=alert(1)> Ready for review.',{exact:true}).waitFor();
    assert.match(await page.locator('#detail').innerText(),/Notification disabled/);
    assert.equal(await page.locator('#detail script, #detail img, #detail svg').count(),0);
    assert.equal(await page.evaluate(()=>globalThis.xss),undefined);
    await page.getByLabel('View',{exact:true}).selectOption('tree');
    assert.equal(await page.locator('.tree-group').count(),1);
    await page.getByLabel('Label',{exact:true}).fill('no-matching-label');
    await page.getByLabel('Label',{exact:true}).press('Tab');
    await page.getByText('No issues match. Create an issue to begin.',{exact:true}).waitFor();
    await page.getByLabel('Label',{exact:true}).fill('');await page.getByLabel('Label',{exact:true}).press('Tab');
    await page.locator('.tree-group').waitFor();
    const tunnelUrl=`http://127.0.0.1:${tunnel.address().port}/tunnel/`;
    const requests = [];page.on('request',req=>{if(req.url().includes(`:${tunnel.address().port}`))requests.push(req.url());});
    await page.goto(tunnelUrl+'#issue=1');
    await page.getByRole('heading',{name:title,exact:true}).waitFor();
    assert(requests.some(url=>url.includes('/tunnel/app.js')));
    assert(requests.some(url=>url.includes('/tunnel/v1/issues')));
    await page.getByLabel('Comment (text / Markdown source)',{exact:true}).fill('Tunnel comment');
    await page.getByRole('button',{name:'Post comment',exact:true}).click();
    await page.locator('.comment-body').getByText('Tunnel comment',{exact:true}).waitFor();
    assert(await page.evaluate(()=>document.documentElement.scrollWidth <= innerWidth), 'Horizontal overflow');
    await mkdir(screenshotDir,{recursive:true});
    await page.screenshot({path:`${screenshotDir}/issues-${width}-light.png`,fullPage:true});
    await page.getByRole('button',{name:'Theme',exact:true}).click();
    await page.screenshot({path:`${screenshotDir}/issues-${width}-dark.png`,fullPage:true});
    assert.deepEqual(errors,[]);assert.equal(stderr,'');
  } finally {
    await browser.close();await new Promise(resolve=>tunnel.close(resolve));fixture.kill();lines.close();await once(fixture,'exit');
  }
});
