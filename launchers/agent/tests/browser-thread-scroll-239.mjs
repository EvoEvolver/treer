import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
const {expect}=createRequire(process.env.LAUNCHER_PLAYWRIGHT_FROM || new URL('../package.json',import.meta.url))('@playwright/test');

// Caller owns the server/browser. These isolated HTTP/WS presentation inputs
// prove scrolling/rendering only, NOT the backend provider/receipt projection.
export async function testThreadScroll239({browser,base,snapshot,screenshots}) {
 const results=[];
 const turn=(index)=>({id:`scroll239-${index}`,startedAt:new Date(1700000000000+index*1000).toISOString(),status:'completed',error:null,
  items:[{id:`user-${index}`,kind:'userMessage',text:`Human turn ${String(index).padStart(3,'0')}`},
         {id:`reply-${index}`,kind:'agentMessage',text:`Provider reply ${String(index).padStart(3,'0')}\n\n${'Bounded synthetic history paragraph. '.repeat(10)}`}]});
 const initial={...snapshot,ready:true,history:{limit:200,cursor:null,totalTurns:180},
  thread:{...snapshot.thread,id:'thread-scroll239',status:'idle',activeTurnId:null,hasOlderItems:false,turns:Array.from({length:180},(_,i)=>turn(i))}};
 for(const width of [1280,390]) {
  const context=await browser.newContext({viewport:{width,height:900},reducedMotion:'reduce'});
  try {
   const page=await context.newPage();const errors=[];page.on('pageerror',e=>errors.push(e.message));
   let socket,reads=0,current=initial;
   await page.routeWebSocket('**/ws',ws=>{socket=ws;});
   await page.route('**/api/state',async route=>{reads++;await new Promise(r=>setTimeout(r,350));await route.fulfill({json:current});});
   await page.goto(base+'/index.html?lang=en');
   const thread=page.locator('#thread');
   await expect(thread).toContainText('Provider reply 179');
   const distance=()=>thread.evaluate(e=>e.scrollHeight-e.clientHeight-e.scrollTop);
   await expect.poll(distance,{message:'async initial history must settle at bottom'}).toBeLessThan(24);
   // An actual user wheel moves the reader; no forced bottom in the assertion.
   await thread.hover();await page.mouse.wheel(0,-1100);
   await expect.poll(distance).toBeGreaterThan(250);
   const before=await thread.evaluate(e=>e.scrollTop);
   current={...initial,thread:{...initial.thread,turns:[...initial.thread.turns,turn(180)]}};
   assert.ok(socket,'caller UI must connect its real state WebSocket route');socket.send(JSON.stringify({...current,type:'state'}));
   await page.waitForTimeout(300);
   const after=await thread.evaluate(e=>e.scrollTop);
   assert.ok(Math.abs(after-before)<60,`new state moved an active reader: ${before}->${after}`);
   const jump=page.getByRole('button',{name:/latest/i});await expect(jump).toBeVisible();await jump.click();
   await expect(thread).toContainText('Provider reply 180');await expect.poll(distance).toBeLessThan(24);
   const user=thread.getByText('Human turn 180',{exact:true}),reply=thread.getByText(/Provider reply 180/);
   await expect(user).toHaveCount(1);await expect(reply).toHaveCount(1);
   assert.ok(await user.evaluate((a)=>{const b=[...document.querySelectorAll('.chat *')].find(e=>e.textContent?.startsWith('Provider reply 180'));return b&&Boolean(a.compareDocumentPosition(b)&Node.DOCUMENT_POSITION_FOLLOWING);}), 'user turn must precede its provider response');
   await page.reload();await expect(thread).toContainText('Provider reply 180');await expect.poll(distance).toBeLessThan(24);
   assert.ok(reads>=2,'refresh must perform a fresh history read');
   assert.deepEqual(errors,[]);
   if(screenshots)await page.screenshot({path:`${screenshots}/thread239-${width}.png`});
   results.push({width,initial_async_bottom:true,user_scroll_preserved:true,jump_latest:true,refresh_bottom:true,duplicate_markers:false,scope:'presentation-only synthetic provider-shaped turns'});
  } finally {await context.close();}
 }
 return results;
}
