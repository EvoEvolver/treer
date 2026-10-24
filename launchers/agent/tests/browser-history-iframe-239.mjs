import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
const {expect}=createRequire(process.env.LAUNCHER_PLAYWRIGHT_FROM || new URL('../package.json',import.meta.url))('@playwright/test');

// Caller owns an isolated real launcher/provider and the parent preview server.
// This module never fabricates state, WS frames or receipts. fixtureHeaders
// are supplied only by the closed fakePrivateState fixture (never live auth).
// Diagnostic results retain false predicates; callers must not label them PASS.
// parentURL(mode) must embed base/index.html in #launcher-frame, initially
// visible, display:none, or height:0 for the corresponding mode.
export async function testHistoryIframe239({browser,base,parentURL,expectedOrder,screenshots,fixtureHeaders}) {
 assert.ok(['127.0.0.1','localhost'].includes(new URL(base).hostname),'isolated loopback launcher only');
 const results=[];
 for(const width of [1280,390]) for(const mode of ['visible','hidden','zero-height']) {
  const context=await browser.newContext({viewport:{width,height:900},reducedMotion:'reduce',extraHTTPHeaders:fixtureHeaders});
  try {
   const page=await context.newPage(),errors=[];page.on('pageerror',e=>errors.push(e.message));
   await page.goto(parentURL(mode),{waitUntil:'domcontentloaded'});
   const iframe=page.locator('#launcher-frame');
   await expect(iframe).toHaveAttribute('src',base+'/index.html?lang=en');
   await expect.poll(()=>page.frames().filter(f=>f.url().startsWith(base+'/index.html')).length).toBe(1);
   let frame=page.frames().find(f=>f.url().startsWith(base+'/index.html'));
   let thread=frame.locator('#thread');
   await expect.poll(()=>thread.locator('.chat .msg').count()).toBeGreaterThan(10);
   const loaded=await thread.evaluate(e=>({height:e.clientHeight,top:e.scrollTop,scrollHeight:e.scrollHeight}));
   if(mode!=='visible') {
    // First AIS data is already rendered in a non-visible iframe. Revealing
    // its real parent matches Console retained tab panels, not a state update.
    await iframe.evaluate(e=>{e.style.display='block';e.style.height='100%';});
   }
   await expect(thread).toBeVisible();
   const distance=()=>thread.evaluate(e=>e.scrollHeight-e.clientHeight-e.scrollTop);
   let initialBottom=true;try {await expect.poll(distance,{timeout:3000}).toBeLessThan(24);}catch {initialBottom=false;}
   const initialDistance=await distance();
   const texts=(await thread.locator('.chat .msg .body').allTextContents()).map(v=>v.trim());
   let cursor=-1;
   for(const text of expectedOrder) {const next=texts.findIndex((v,i)=>i>cursor&&v===text);assert.ok(next>cursor,`missing/reordered actual projected message: ${text}`);assert.equal(texts.filter(v=>v===text).length,1,`duplicate projected user/provider text: ${text}`);cursor=next;}
   // Latest is used only AFTER capturing the first-open result; never repairs
   // the initial position before measuring it.
   if(!initialBottom) {const jump=frame.locator('#jump');if(await jump.isVisible())await jump.click();}
   await thread.hover();await page.mouse.wheel(0,-900);
   await expect.poll(distance).toBeGreaterThan(250);
   const before=await thread.evaluate(e=>e.scrollTop);
   await page.setViewportSize({width,height:760});
   await page.waitForTimeout(250);
   const after=await thread.evaluate(e=>e.scrollTop);
   const readerPreserved=Math.abs(after-before)<60;
   // Browser refresh restores nested frame session history differently from a
   // new iframe navigation. Measure the actual parent reload after reading up.
   await page.reload({waitUntil:'domcontentloaded'});
   await expect.poll(()=>page.frames().filter(f=>f.url().startsWith(base+'/index.html')).length).toBe(1);
   frame=page.frames().find(f=>f.url().startsWith(base+'/index.html'));thread=frame.locator('#thread');
   await expect.poll(()=>thread.locator('.chat .msg').count()).toBeGreaterThan(10);
   if(mode!=='visible')await page.locator('#launcher-frame').evaluate(e=>{e.style.display='block';e.style.height='100%';});
   let parentReloadBottom=true;try {await expect.poll(distance,{timeout:3000}).toBeLessThan(24);}catch {parentReloadBottom=false;}
   await frame.goto(base+'/index.html?lang=en');
   await expect.poll(()=>frame.locator('#thread .chat .msg').count()).toBeGreaterThan(10);
   let reloadBottom=true;try {await expect.poll(distance,{timeout:3000}).toBeLessThan(24);}catch {reloadBottom=false;}
   if(screenshots)await page.screenshot({path:`${screenshots}/iframe239-${width}-${mode}.png`});
   assert.deepEqual(errors,[]);
   results.push({width,mode,loaded,initialBottom,initialDistance,readerPreserved,reloadBottom,parentReloadBottom,actualProjectionOrder:true,scope:'real AIS/provider receipt projection inside local iframe; not live workspace proof'});
  } finally {await context.close();}
 }
 return results;
}

// A bounded real provider history (>100 turns) exercises same-thread page
// replacement; it must not be confused with initial document placement.
export async function testHistoryPage239({browser,base,parentURL,fixtureHeaders}) {
 assert.ok(['127.0.0.1','localhost'].includes(new URL(base).hostname));
 const context=await browser.newContext({viewport:{width:1280,height:900},reducedMotion:'reduce',extraHTTPHeaders:fixtureHeaders});
 try {
  const page=await context.newPage();await page.goto(parentURL('visible'));
  const frame=page.frames().find(f=>f.url().startsWith(base+'/index.html'));
  const thread=frame.locator('#thread'),distance=()=>thread.evaluate(e=>e.scrollHeight-e.clientHeight-e.scrollTop);
  await expect.poll(distance).toBeLessThan(24);
  const initial=await distance();
  await frame.getByRole('button',{name:'Load earlier messages',exact:true}).click();
  const latest=frame.getByRole('button',{name:'Back to latest messages',exact:true});
  await expect(latest).toBeVisible();
  await thread.hover();await page.mouse.wheel(0,600);
  await latest.click();
  await expect(latest).toHaveCount(0);
  let latestBottom=true;try {await expect.poll(distance,{timeout:3000}).toBeLessThan(24);}catch {latestBottom=false;}
  return {initialDistance:initial,latestBottom,latestDistance:await distance(),scope:'real provider history bounded-page return; separate from first-open'};
 }finally {await context.close();}
}

// Against a caller-owned legacy journal fixture, actual late native echoes
// resolve the placement. No UI state or provider message is synthesized here.
export async function testLegacyTransition239({page,base,parentURL,screenshot}) {
 assert.ok(['127.0.0.1','localhost'].includes(new URL(base).hostname));
 await page.goto(parentURL('visible'),{waitUntil:'domcontentloaded'});
 await expect.poll(()=>page.frames().filter(f=>f.url().startsWith(base+'/index.html')).length).toBe(1);
 const frame=page.frames().find(f=>f.url().startsWith(base+'/index.html'));
 const thread=frame.locator('#thread');
 await expect(thread.locator('.position-note')).toHaveCount(2);
 const pending=(await thread.locator('.msg .body').allTextContents()).map(s=>s.trim());
 assert.deepEqual(pending,['initial question','before steering','after steering 0','after steering 1','steering question 0','steering question 1']);
 await page.setViewportSize({width:390,height:900});
 const mobileOverflow=await frame.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1);
 assert.equal(mobileOverflow,false,'retained-position notice must fit mobile');
 await expect(thread.locator('.position-note')).toHaveCount(0,{timeout:5000});
 const final=(await thread.locator('.msg .body').allTextContents()).map(s=>s.trim());
 assert.deepEqual(final,['initial question','before steering','steering question 0','after steering 0','steering question 1','after steering 1']);
 const state=await frame.evaluate(()=>fetch('api/state').then(r=>r.json()));
 assert.notEqual(state.runtime.state,'blocked');
 assert.equal(state.thread.turns[0].items.filter(i=>i.kind==='userMessage').length,3);
 assert.ok(state.thread.turns[0].items.every(i=>!i.positionUnavailable));
 if(screenshot)await page.screenshot({path:screenshot});
 return {pendingPositionNotes:2,finalPositionNotes:0,mobileOverflow:false,nativeOrder:true,duplicateUsers:false,runtimeBlocked:false,scope:'actual isolated Rust/provider/legacy receipt transition in iframe'};
}

// Replay two unmodified caller-owned actual AIS captures, not hand-built turns.
// Only transport ordering is controlled: first WS is fresh, older initial HTTP
// resolves after the fresh marker has actually rendered inside the iframe.
export async function testInitialSnapshotRace239({browser,base,parentURL,older,fresh,marker,screenshots}) {
 assert.equal(older.history.limit,fresh.history.limit,'same limit reproduces the initial-response race');
 const results=[];
 for(const width of [1280,390]) {
  const context=await browser.newContext({viewport:{width,height:900},reducedMotion:'reduce'});
  let release;const gate=new Promise(r=>{release=r;});
  try {
   const page=await context.newPage(),errors=[];page.on('pageerror',e=>errors.push(e.message));
   await page.route('**/api/state',async route=>{await gate;await route.fulfill({json:older});});
   await page.routeWebSocket('**/ws',ws=>ws.send(JSON.stringify({...fresh,type:'state'})));
   await page.goto(parentURL('visible'),{waitUntil:'domcontentloaded'});
   await expect.poll(()=>page.frames().filter(f=>f.url().startsWith(base+'/index.html')).length).toBe(1);
   const frame=page.frames().find(f=>f.url().startsWith(base+'/index.html'));
   const thread=frame.locator('#thread'),latest=thread.getByText(marker,{exact:true});
   await expect(latest).toHaveCount(1);
   const response=page.waitForResponse(r=>r.url().endsWith('/api/state'));
   release();await response;
   await frame.evaluate(()=>new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r))));
   await page.waitForTimeout(100);
   const latestRetained=await latest.count()===1;
   const distance=await thread.evaluate(e=>e.scrollHeight-e.clientHeight-e.scrollTop);
   const overflow=await frame.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1);
   if(screenshots)await page.screenshot({path:`${screenshots}/initial-race239-${width}.png`});
   assert.deepEqual(errors,[]);
   results.push({width,latestRetained,distance,overflow,scope:'actual-AIS-capture replay with controlled initial HTTP/WS ordering; not live auth'});
  }finally {release();await context.close();}
 }
 return results;
}
