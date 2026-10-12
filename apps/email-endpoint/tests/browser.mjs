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
  const {url,thread}=JSON.parse(first);
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
  const context=await browser.newContext({extraHTTPHeaders:{'X-Treer-Identity-Token':'other-human'}});
  const page=await context.newPage();const response=await page.goto(`${url}/t/${thread}/`);
  assert.equal(response.status(),403);await page.getByRole('heading',{name:'Access denied'}).waitFor();
  console.log(`Browser checks passed: desktop, 390px dark, XSS, replies, denial, tunnel assets. Screenshots: ${artifacts}`);
} finally {await browser?.close();child.kill();lines.close();}
