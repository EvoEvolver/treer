import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
const {chromium} = await import(process.env.SOUL_PLAYWRIGHT_MODULE || '/Users/dooms/treer/apps/paper/node_modules/playwright/index.mjs');
const root=fileURLToPath(new URL('../',import.meta.url));
for(const width of [1280,390]) for(const tunnel of [false,true]) {
  test(`human auto identity ${tunnel?'iframe tunnel':'ingress'} ${width}px`,async()=>{
    const processHandle=spawn('python3',[root+'tests/fixture.py'],{env:{PATH:'/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin',HOME:'/nonexistent',PYTHONUTF8:'1'},stdio:['ignore','pipe','pipe']});
    let errors=''; processHandle.stderr.on('data',b=>errors+=b);
    const ready = await new Promise((resolve,reject)=>{
      createInterface({input:processHandle.stdout}).once('line',line=>resolve(JSON.parse(line)));
      processHandle.once('exit',()=>reject(new Error(errors)));
    });
    const browser=await chromium.launch({headless:true,env:{PATH:'/usr/bin:/bin',HOME:'/tmp'}});
    try {
      const page=await browser.newPage({viewport:{width,height:1000},colorScheme:width===390?'dark':'light'});
      const errors=[];page.on('pageerror',e=>errors.push(e));
      await page.goto(ready.url+(tunnel?'/control':'/'));
      const context=tunnel?page.frameLocator('iframe'):page;
      await context.locator('.soul-row').waitFor();
      assert.equal(await context.locator('#login').isVisible(),false);
      assert.equal(await context.locator('.soul-row strong').textContent(),'<img src=x onerror=alert(1)>');
      assert.equal(await context.locator('img').count(),0);
      assert.match(await context.locator('#detail').innerText(),/u_human/);
      assert.match(await context.locator('.manifest').first().innerText(),/schema_version/);
      assert.equal(await context.locator('body').evaluate(el=>el.scrollWidth<=el.clientWidth),true);
      const screenshots=process.env.SOUL_SCREENSHOT_DIR||'/tmp/treer-soul-v2-screenshots';
      await mkdir(screenshots,{recursive:true});
      await page.screenshot({path:`${screenshots}/${tunnel?'embedded':'ingress'}-${width}.png`,fullPage:true});
      page.once('dialog',dialog=>dialog.accept());
      await context.getByRole('button',{name:'Delete soul'}).click();
      await context.getByText('No souls stored',{exact:true}).waitFor();
      assert.deepEqual(errors,[]);
    } finally {
      await browser.close();processHandle.kill('SIGTERM');
    }
  });
}
