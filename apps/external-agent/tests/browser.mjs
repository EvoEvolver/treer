import { pathToFileURL } from 'node:url';
const modulePath=process.env.EXTERNAL_PLAYWRIGHT_MODULE;
if(!modulePath)throw new Error('Set EXTERNAL_PLAYWRIGHT_MODULE to the installed Playwright entry');
const { chromium }=await import(pathToFileURL(modulePath));
const browser=await chromium.launch({headless:true});
try {
 for(const [width,height,colorScheme,language] of [[1100,850,'light','en'],[390,844,'dark','zh']]) {
  const context=await browser.newContext({viewport:{width,height},colorScheme,locale:language});const page=await context.newPage();
  const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const response=await page.goto(process.argv[2]+'/');
  const headers=response.headers();if(!headers['content-security-policy'].includes("script-src 'self'")||headers['cache-control']!=='no-store')throw new Error('Missing actual App response protections');
  await page.locator('#controls').waitFor({state:'visible'});
  await page.selectOption('#language',language);
  const name=`<img src=x onerror=alert(1)> ${language}`;
  await page.locator('#name').fill(name);
  await page.locator('#authority').check();await page.locator('#create-button').click();
  await page.locator('#secret-area').waitFor({state:'visible'});
  if(!(await page.locator('#secret').inputValue()).startsWith('ext1_'))throw new Error('No create-once secret');
  await page.locator('#dismiss').click();if(await page.locator('#secret').inputValue())throw new Error('Secret not cleared');
  if(await page.locator('#tokens img').count())throw new Error('Token name interpreted as markup');
  page.once('dialog',dialog=>dialog.accept());
  const row=page.locator('.token').filter({hasText:name});await row.locator('button').click();
  await page.waitForFunction(name=>[...document.querySelectorAll('.token')].some(e=>e.textContent.includes(name)&&e.querySelector('button')?.disabled),name);
  if(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth))throw new Error('Horizontal overflow');
  await page.keyboard.press('Tab');if(await page.evaluate(()=>document.activeElement===document.body))throw new Error('Keyboard focus missing');
  if(errors.length)throw new Error('Browser runtime error');
  await context.close();
 }
 console.log('Chromium local gateway UI PASS: create once, safe names, clear, revoke, en/zh, desktop/narrow, light/dark; not canary acceptance');
} finally {await browser.close();}
