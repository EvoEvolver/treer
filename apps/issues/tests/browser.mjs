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
    assert.match(await page.locator('#detail .assignee-trigger').innerText(), /Unplaced\s+ag_no_path/);
    await assign(page,'ag_a');
    assert.match(await page.locator('#detail .assignee-trigger').innerText(), /\/engineering\/backend\/Builder/);
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'Selected full path fits narrow screen');
    await page.getByLabel('Labels (comma separated)',{exact:true}).fill('team, backend');
    await page.getByRole('button',{name:'Create issue',exact:true}).click();
    await page.getByRole('heading',{name:title,exact:true}).waitFor();
    for(const [id,path] of [['ag_b','/engineering/review/Reviewer'],['ag_c','/design/Reviewer']]) {
      await assign(page,id);
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
      await surface.waitForFunction(()=>document.querySelector('#detail > p.meta')?.textContent.startsWith('done ·'));
      await surface.locator('#detail').getByRole('link',{name:'#1',exact:true}).click();
      await surface.getByRole('heading',{name:/Children · 1\/1/}).waitFor();
      await assign(surface,'usr_a');
      await surface.locator('#detail').getByLabel('Status',{exact:true}).selectOption('in_progress');
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


async function seed(config) {
  const agent = (token, body) => fetch(config.url + 'v1/issues', {method: 'POST', headers: {'Content-Type': 'application/json', 'X-Treer-Identity-Token': token}, body: JSON.stringify(body)}).then(r => r.json());
  return {agent};
}

for (const width of [1280, 390]) test(`close buttons, keyboard, permissions and screenshots at ${width}px`, async () => {
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
    const create = body => page.evaluate(async body => (await (await fetch(new URL('v1/issues',document.baseURI),{method:'POST',headers:{'Content-Type':'application/json','X-Issues-Request':'1'},body:JSON.stringify(body)})).json()).issue.id, body);
    const status = id => fetch(`${config.url}v1/issues/${id}`,{headers:{'X-Treer-Identity-Token':'agent-a'}}).then(r=>r.json()).then(r=>r.issue.status);
    await create({title:'Linear-style Issues UI',priority:'high',labels:['ui'],assignee:'ag_a',status:'in_progress'});
    await create({title:'Hourly assignee reminders',priority:'urgent',labels:['backend'],assignee:'ag_b'});
    await create({title:'One-click Close button',labels:['ui'],parent_id:1,assignee:'usr_a'});
    await create({title:'Dark theme polish',priority:'low',parent_id:1,status:'done'});
    await create({title:'Mobile layout',parent_id:1,status:'blocked'});
    await create({title:'Write release notes',priority:'low',labels:['docs']});
    await create({title:'Old spike',status:'closed'});
    const owned = (await (await seed(config)).agent('agent-b',{title:'Agent-owned issue',assignee:'ag_b'})).issue.id;
    await page.reload();
    await page.locator('#list .issue-row').first().waitFor();
    // Done and closed start collapsed, so the active work is dense and uncluttered.
    assert.equal(await page.locator('#list .issue-row').count(),6);
    await page.locator('.status-group',{hasText:/closed\s*1/}).waitFor();
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'List fits the viewport');
    await mkdir(screenshotDir,{recursive:true});
    await page.screenshot({path:`${screenshotDir}/issues-list-${width}-light.png`,fullPage:true});
    await page.getByRole('button',{name:'Theme',exact:true}).click();
    await page.screenshot({path:`${screenshotDir}/issues-list-${width}-dark.png`,fullPage:true});
    await page.getByRole('button',{name:'Theme',exact:true}).click();

    // "Needs me": assigned to me and unresolved, combinable with the label filter.
    await page.locator('#nav-needs').click();
    await page.waitForFunction(()=>document.querySelectorAll('#list .issue-row').length===1);
    assert.match(await page.locator('#list .issue-row').first().getAttribute('aria-label'),/^ISS-3 /);
    assert.equal(await page.locator('#list-title').innerText(),'Needs me');
    await page.getByLabel('Label',{exact:true}).fill('docs');await page.getByLabel('Label',{exact:true}).press('Enter');
    await page.getByText('No issues match. Create an issue to begin.',{exact:true}).waitFor();
    await page.getByLabel('Label',{exact:true}).fill('ui');await page.getByLabel('Label',{exact:true}).press('Enter');
    await page.waitForFunction(()=>document.querySelectorAll('#list .issue-row').length===1);
    await page.screenshot({path:`${screenshotDir}/issues-needs-me-${width}.png`,fullPage:true});
    await page.locator('#nav-all').click();
    await page.waitForFunction(()=>document.querySelectorAll('#list .issue-row').length===6);

    // One-click Close from a list row, with undo.
    await page.getByRole('button',{name:'Close ISS-6',exact:true}).click();
    await page.getByRole('status').getByText('Closed ISS-6').waitFor();
    assert.equal(await status(6),'closed');
    await page.getByRole('status').getByRole('button',{name:'Undo'}).click();
    await page.waitForFunction(()=>document.querySelector('#list [data-issue-id="6"]'));
    assert.equal(await status(6),'open');

    // Permissions mirror the backend: Alice may not edit an Agent-owned issue, so Close is disabled.
    assert.equal(await page.getByRole('button',{name:`Close ISS-${owned}`,exact:true}).isDisabled(),true);

    // Keyboard: j/k move the focus ring, x closes the focused issue, Enter opens, ? shows help.
    await page.locator('body').press('j');
    assert.equal(await page.locator('.issue-line.kb').count(),1);
    const first = await page.locator('.issue-line.kb').getAttribute('data-issue-id');
    await page.locator('body').press('j');
    assert.notEqual(await page.locator('.issue-line.kb').getAttribute('data-issue-id'),first);
    await page.locator('body').press('k');
    assert.equal(await page.locator('.issue-line.kb').getAttribute('data-issue-id'),first);
    await page.locator('body').press('Enter');
    await page.waitForFunction(id=>document.querySelector('#detail h2')&&document.querySelector('.issue-line.selected')?.dataset.issueId===id,first);
    assert.equal(await page.locator('.issue-line.selected').getAttribute('data-issue-id'),first);
    // The detail view explains why a read-only issue cannot be closed instead of silently disabling the button.
    assert.equal(first,String(owned));
    assert.equal(await page.getByRole('button',{name:'Close issue',exact:true}).isDisabled(),true);
    await page.locator('#detail').getByText(/You can comment, but only the creator/).waitFor();
    await page.locator('body').press('?');
    await page.getByRole('dialog',{name:'Keyboard shortcuts'}).waitFor();
    await page.locator('body').press('Escape');
    await page.getByRole('dialog',{name:'Keyboard shortcuts'}).waitFor({state:'hidden'});

    // Close and reopen from the detail view of an issue Alice may edit.
    await page.locator('body').press('Escape');
    await page.locator('[data-issue-id="6"] .issue-row').click();
    await page.getByRole('heading',{name:'Write release notes',exact:true}).waitFor();
    await page.getByRole('button',{name:'Close issue',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#detail > p.meta')?.textContent.startsWith('closed ·'));
    assert.equal(await status(6),'closed');
    await page.getByRole('button',{name:'Reopen issue',exact:true}).click();
    await page.waitForFunction(()=>document.querySelector('#detail > p.meta')?.textContent.startsWith('open ·'));
    assert.equal(await status(6),'open');
    await page.screenshot({path:`${screenshotDir}/issues-detail-${width}-light.png`,fullPage:true});
    await page.getByRole('button',{name:'Theme',exact:true}).click();
    await page.screenshot({path:`${screenshotDir}/issues-detail-${width}-dark.png`,fullPage:true});

    // Keyboard close on the focused row; Escape returns to the list on narrow screens.
    await page.locator('body').press('Escape');
    await page.locator('body').press('j');
    const target = await page.locator('.issue-line.kb').getAttribute('data-issue-id');
    await page.locator('body').press('x');
    await page.getByRole('status').getByText(`Closed ISS-${target}`).waitFor();
    assert.equal(await status(Number(target)),'closed');
    assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'No horizontal overflow');
    assert.deepEqual(errors,[]);assert.equal(stderr,'');
  } finally {
    await browser.close();fixture.kill();lines.close();await once(fixture,'exit');
  }
});
