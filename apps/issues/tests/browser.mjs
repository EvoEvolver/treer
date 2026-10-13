import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
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

async function assign(surface,id) {
  await surface.locator('#detail').getByRole('button',{name:'Assignee',exact:true}).click();
  await surface.locator(`[data-principal-id="${id}"]`).click();
}

for (const width of [1280, 390]) test(`OAuth fallback with a contract double, issue workflow and XSS at ${width}px`, async () => {
  const fixture = spawn(process.env.PYTHON || 'python3', [fileURLToPath(new URL('./fixture.py', import.meta.url))], {env:hermeticEnv(), stdio:['ignore','pipe','pipe']});
  let stderr = ''; fixture.stderr.on('data',chunk => stderr += chunk);
  const lines = createInterface({ input: fixture.stdout });
  const [line] = await once(lines, 'line'); const config = JSON.parse(line);
  const errors = [];
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
    await page.locator('#detail').getByRole('button',{name:'Assignee',exact:true}).click();
    const picker=page.getByRole('dialog',{name:'Choose assignee'});
    assert.equal(await picker.getByText('PEOPLE',{exact:true}).count(),1);
    assert.equal(await picker.getByText('AGENT TREE',{exact:true}).count(),1);
    assert.equal(await picker.locator('details details').count(),2,'Nested folders preserve Agent tree');
    assert.equal(await picker.locator('[data-principal-id="usr_a"]').count(),1);
    assert.equal(await picker.locator('[data-principal-id="ag_no_path"]').count(),1,'Legacy directory remains usable');
    await picker.locator('summary').filter({hasText:'engineering'}).click();
    assert.equal(await picker.locator('[data-principal-id="ag_a"]').isVisible(),false);
    await picker.locator('summary').filter({hasText:'engineering'}).click();
    await picker.getByRole('searchbox',{name:'Search assignees'}).fill('Reviewer');
    assert.equal(await picker.locator('[data-principal-id^="ag_"]').count(),2,'Same names retain distinct stable IDs');
    await picker.getByRole('searchbox',{name:'Search assignees'}).fill('/engineering/review');
    assert.equal(await picker.locator('[data-principal-id="ag_b"]').count(),1);
    assert.equal(await picker.locator('[data-principal-id="ag_c"]').count(),0);
    await picker.getByRole('searchbox',{name:'Search assignees'}).fill('');
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'Open picker fits narrow screen');
    await mkdir(screenshotDir,{recursive:true});
    await page.screenshot({path:`${screenshotDir}/issues-assignee-${width}.png`,fullPage:true});
    await picker.getByRole('searchbox',{name:'Search assignees'}).press('Escape');
    assert.equal(await picker.isVisible(),false);
    await assign(page,'ag_b');
    assert.match(await page.locator('#detail .assignee-trigger').innerText(), /Reviewer[\s\S]*\/engineering\/review\/Reviewer/);
    await assign(page,'ag_c');
    assert.match(await page.locator('#detail .assignee-trigger').innerText(), /Reviewer[\s\S]*\/design\/Reviewer/);
    assert.doesNotMatch(await page.locator('#detail .assignee-trigger').innerText(), /engineering/);
    await assign(page,'ag_no_path');
    assert.match(await page.locator('#detail .assignee-trigger').innerText(), /Path unavailable · ag_no_path/);
    await assign(page,'ag_a');
    assert.match(await page.locator('#detail .assignee-trigger').innerText(), /\/engineering\/backend\/Builder/);
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'Selected full path fits narrow screen');
    await page.getByLabel('Labels (comma separated)',{exact:true}).fill('team, backend');
    await page.getByRole('button',{name:'Create issue',exact:true}).click();
    await page.getByRole('heading',{name:title,exact:true}).waitFor();
    for(const [id,path] of [['ag_b','/engineering/review/Reviewer'],['ag_c','/design/Reviewer']]) {
      await assign(page,id);
      await page.getByRole('button',{name:'Save status / assignee',exact:true}).click();
      await page.waitForFunction(path=>document.querySelector('#list .issue-row')?.getAttribute('aria-label').includes(path) && document.querySelector('#detail .selected-name .person-path')?.textContent===path && !document.querySelector('#detail .assignee-trigger')?.disabled,path);
      const saved=await page.evaluate(async()=> (await (await fetch(new URL('v1/issues/1',document.baseURI))).json()).issue.assignee);
      assert.equal(saved,id,'Same-name selections persist the distinct Agent ID');
      assert.match(await page.locator('#list .issue-row').first().getAttribute('aria-label'),new RegExp(path));
    }
    await page.screenshot({path:`${screenshotDir}/issues-selected-path-${width}.png`,fullPage:true});
    await page.getByRole('button',{name:'Add child',exact:true}).click();
    await page.getByLabel('Title',{exact:true}).fill('Reproduce build failure');
    await page.getByRole('button',{name:'Create issue',exact:true}).click();
    await page.getByRole('heading',{name:'Reproduce build failure',exact:true}).waitFor();
    await page.locator('#detail').getByLabel('Status',{exact:true}).selectOption('done');
    await page.getByRole('button',{name:'Save status / assignee',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#detail > p.meta')?.textContent.startsWith('done ·'));
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
    assert(await page.evaluate(()=>document.documentElement.scrollWidth <= innerWidth), 'Horizontal overflow');
    await mkdir(screenshotDir,{recursive:true});
    await page.screenshot({path:`${screenshotDir}/issues-oauth-${width}-light.png`,fullPage:true});
    await page.getByRole('button',{name:'Theme',exact:true}).click();
    await page.screenshot({path:`${screenshotDir}/issues-oauth-${width}-dark.png`,fullPage:true});
    assert.deepEqual(errors,[]);assert.equal(stderr,'');
  } finally {
    await browser.close();fixture.kill();lines.close();await once(fixture,'exit');
  }
});


for (const mode of ['embedded', 'ingress']) for (const width of [1280, 390]) {
  test(`already logged-in human opens ${mode}, no App OAuth, ${width}px`, async () => {
    const fixture = spawn(process.env.PYTHON || 'python3', [fileURLToPath(new URL('./fixture.py', import.meta.url)), '--browser-identity'], {env:hermeticEnv(), stdio:['ignore','pipe','pipe']});
    let stderr = '';fixture.stderr.on('data',chunk=>stderr+=chunk);
    const lines = createInterface({input:fixture.stdout});const [line]=await once(lines,'line');const config=JSON.parse(line);
    const browser=await chromium.launch({env:hermeticEnv()});
    try {
      const context=await browser.newContext({viewport:{width,height:900}});
      await context.addCookies([
        {name:'treer_session',value:'test-login',url:config.proxy},
        {name:'treer_test_ingress',value:'test-login',url:config.ingress},
      ]);
      // Simulate a malicious browser sending identity headers. The gateway must
      // discard these before injecting the current session's verified human.
      await context.setExtraHTTPHeaders({'X-Treer-Identity-Token':'forged','X-Treer-Agent-ID':'ag_b'});
      const page=await context.newPage();page.setDefaultTimeout(8000);
      const errors=[],requests=[];page.on('pageerror',e=>errors.push(e.message));page.on('request',r=>requests.push(r.url()));
      const root=config[mode];
      async function appSurface() {
        if(mode!=='embedded') return page;
        await page.locator('iframe').waitFor();
        await page.frameLocator('iframe').getByRole('button',{name:'New issue',exact:true}).waitFor();
        return page.frames().find(frame=>frame.url().startsWith(config.embedded));
      }
      await page.goto(mode==='embedded'?config.control_plane:root);
      let surface=await appSurface();
      await surface.getByRole('button',{name:'New issue',exact:true}).waitFor();
      assert.equal(await surface.locator('#login').isVisible(),false);
      assert.equal(await surface.locator('#identity').textContent(),'Alice');
      assert(!requests.some(url=>url.includes('/oauth/') || url.includes('/api/auth/')));
      await surface.getByRole('button',{name:'New issue',exact:true}).click();
      const title='Ship automatically authenticated <img src=x onerror=alert(1)> issues';
      await surface.getByLabel('Title',{exact:true}).fill(title);
      await surface.getByLabel('Description (Markdown source, rendered as text)',{exact:true}).fill('## Plan\n<script>window.xss=1</script>\nUse the existing Treer session; no second sign-in.');
      await assign(surface,'ag_b');
      await surface.getByLabel('Labels (comma separated)',{exact:true}).fill('team, auth');
      await surface.getByRole('button',{name:'Create issue',exact:true}).click();
      await surface.getByRole('heading',{name:title,exact:true}).waitFor();
      await surface.getByRole('button',{name:'Add child',exact:true}).click();
      await surface.getByLabel('Title',{exact:true}).fill('Verify current user identity');
      await surface.getByRole('button',{name:'Create issue',exact:true}).click();
      await surface.getByRole('heading',{name:'Verify current user identity',exact:true}).waitFor();
      await surface.locator('#detail').getByLabel('Status',{exact:true}).selectOption('done');
      await surface.getByRole('button',{name:'Save status / assignee',exact:true}).click();
      await surface.waitForFunction(()=>document.querySelector('#detail > p.meta')?.textContent.startsWith('done ·'));
      await surface.locator('#detail').getByRole('link',{name:'#1',exact:true}).click();
      await surface.getByRole('heading',{name:/Children · 1\/1/}).waitFor();
      await assign(surface,'usr_a');
      await surface.locator('#detail').getByLabel('Status',{exact:true}).selectOption('in_progress');
      await surface.getByRole('button',{name:'Save status / assignee',exact:true}).click();
      await surface.waitForFunction(()=>document.querySelector('#detail > p.meta')?.textContent.startsWith('in progress ·'));
      await surface.getByRole('button',{name:'Claim for 15 min',exact:true}).click();
      await surface.getByRole('button',{name:'Renew lease',exact:true}).waitFor();
      await surface.getByRole('button',{name:'Renew lease',exact:true}).click();
      await surface.getByText(/Notifications: not_requested/).waitFor();
      await surface.getByRole('button',{name:'Release lease',exact:true}).click();
      await surface.getByRole('button',{name:'Claim for 15 min',exact:true}).waitFor();
      await surface.getByLabel('Comment (text / Markdown source)',{exact:true}).fill('<svg onload=alert(1)> Existing Treer session works.');
      await surface.getByRole('button',{name:'Post comment',exact:true}).click();
      await surface.locator('.comment-body').getByText('<svg onload=alert(1)> Existing Treer session works.',{exact:true}).waitFor();
      await surface.getByLabel('View',{exact:true}).selectOption('tree');assert.equal(await surface.locator('.tree-group').count(),1);
      assert.equal(await surface.locator('#detail script, #detail img, #detail svg').count(),0);
      assert.equal(await surface.evaluate(()=>globalThis.xss),undefined);
      if(mode==='embedded') {await page.goto(config.control_plane+'?issue=1');surface=await appSurface();} else {await page.reload();}
      await surface.getByRole('heading',{name:title,exact:true}).waitFor();
      const records=(await (await fetch(config.proxy+'test/forwarded')).json()).requests;
      assert(records.length>10);assert(records.every(r=>r.ttl===60 && r.sub==='usr_a' && !r.cookie && !r.spoofed_header));
      assert.equal(new Set(records.map(r=>r.jti)).size,records.length,'fresh identity per request');
      assert(records.every(r=>r.embedded===(mode==='embedded')));
      const prefix=mode==='embedded'?'/api/workspaces/ws_test/apps/app_issues/ui/':'/';
      assert(requests.some(url=>url===root+'app.js'));assert(requests.some(url=>url.startsWith(root+'v1/issues')));
      assert(requests.filter(url=>url.startsWith(new URL(root).origin)).every(url=>new URL(url).pathname.startsWith(prefix)));
      assert(!requests.some(url=>url.includes('/oauth/') || url.includes('/api/auth/')));
      assert(!(await context.cookies()).some(cookie=>cookie.name==='treer_issues_session'));
      assert(await surface.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
      if(mode==='embedded') {const height=await surface.evaluate(()=>document.documentElement.scrollHeight);await page.locator('iframe').evaluate((iframe,height)=>iframe.style.height=height+'px',height);}
      await mkdir(screenshotDir,{recursive:true});
      await page.screenshot({path:`${screenshotDir}/issues-${mode}-${width}-light.png`,fullPage:true});
      await surface.getByRole('button',{name:'Theme',exact:true}).click();
      await page.screenshot({path:`${screenshotDir}/issues-${mode}-${width}-dark.png`,fullPage:true});
      assert.deepEqual(errors,[]);assert.equal(stderr,'');
    } finally {await browser.close();fixture.kill();lines.close();await once(fixture,'exit');}
  });
}
