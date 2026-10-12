import {spawn} from 'node:child_process';
import {mkdir} from 'node:fs/promises';
import {createInterface} from 'node:readline';
import assert from 'node:assert/strict';
import {chromium} from 'playwright';
import {hermeticEnv} from '../../agent-ui/tests/hermetic.mjs';
const env=hermeticEnv();
for(const k of Object.keys(env)) if(k.startsWith('EMAIL_ENDPOINT_') || /^(http|https|all)_proxy$/i.test(k)) delete env[k];
const child=spawn('python3',[new URL('./browser_fixture.py',import.meta.url).pathname],{env,stdio:['ignore','pipe','inherit']});
const lines=createInterface({input:child.stdout});
let browser;
try {
  const first=await Promise.race([new Promise(resolve=>lines.once('line',resolve)),new Promise((_,reject)=>setTimeout(()=>reject(new Error('Fixture timeout')),15000))]);
  const {url,thread,gateway}=JSON.parse(first);
  const artifacts=process.env.EMAIL_ENDPOINT_SCREENSHOTS || '/tmp/email-endpoint-browser';await mkdir(artifacts,{recursive:true});
  browser=await chromium.launch({headless:true});
  for(const [name,width,height,scheme] of [['desktop',1280,900,'light'],['mobile',390,844,'dark']]) {
    const context=await browser.newContext({viewport:{width,height},colorScheme:scheme,extraHTTPHeaders:{'X-Treer-Identity-Token':'human'}});
    const page=await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
    await page.goto(`${url}/t/${thread}/`);
    await page.getByRole('heading',{name:'A decision needs your review'}).waitFor();
    assert.equal(await page.evaluate(()=>window.XSS),undefined);
    await page.locator('#body').fill(`Human reply from ${name}`);
    await page.getByRole('button',{name:'Send reply'}).click();
    await page.getByRole('status').filter({hasText:'Message delivered. Agent wake is disabled.'}).waitFor();
    await page.getByText(`Human reply from ${name}`,{exact:true}).waitFor();
    assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
    await page.screenshot({path:`${artifacts}/${name}.png`,fullPage:true});
    await page.goto(`${url}/tunnel/t/${thread}/`);
    await page.getByRole('heading',{name:'A decision needs your review'}).waitFor();
    assert.equal(await page.locator('link[rel="stylesheet"]').getAttribute('href'),`${url}/tunnel/style.css`);
    assert.deepEqual(errors,[]);
    await context.close();
  }
  for (const width of [1280,390]) {
    const context=await browser.newContext({viewport:{width,height:900},extraHTTPHeaders:{'X-Treer-Identity-Token':'forged-client-header'}});
    await context.addCookies([{name:'treer_test_platform',value:'logged-in',url:gateway,httpOnly:true,sameSite:'Lax'}]);
    const page=await context.newPage();const oauthRequests=[];
    page.on('request',r=>{if(r.url().includes('/api/auth/start') || r.url().includes('/oauth/'))oauthRequests.push(r.url());});
    await page.goto(`${gateway}/t/${thread}/`);
    await page.getByRole('heading',{name:'A decision needs your review'}).waitFor();
    assert.equal(await page.locator('#login').isVisible(),false);
    await page.goto(`${gateway}/control-plane/`);
    const frame=page.frameLocator('iframe');
    await frame.getByRole('heading',{name:'A decision needs your review'}).waitFor();
    assert.equal(await frame.locator('#login').isVisible(),false);
    await frame.locator('#body').fill(`Gateway reply ${width}`);
    await frame.getByRole('button',{name:'Send reply'}).click();
    await frame.getByText(`Gateway reply ${width}`,{exact:true}).waitFor();
    assert.deepEqual(oauthRequests,[]);
    assert.equal((await context.cookies()).some(c=>c.name==='treer_email_endpoint_session'),false);
    await page.screenshot({path:`${artifacts}/gateway-${width}.png`,fullPage:true});
    await context.close();
  }
  const stats=await (await fetch(`${gateway}/gateway/stats`)).json();
  assert.ok(stats.verified>10);assert.ok(stats.issued>=stats.verified);assert.equal(stats.oauth_requests,0);
  const context=await browser.newContext({extraHTTPHeaders:{'X-Treer-Identity-Token':'other-human'}});
  const page=await context.newPage();const response=await page.goto(`${url}/t/${thread}/`);
  assert.equal(response.status(),403);await page.getByRole('heading',{name:'Access denied'}).waitFor();
  console.log(`Browser checks passed: desktop, 390px dark, XSS, replies, denial, signed per-request gateway identity, iframe/tunnel assets, no OAuth/cookie login. Screenshots: ${artifacts}`);
} finally {await browser?.close();child.kill();lines.close();}
