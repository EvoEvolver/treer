// Synthetic HTTP owner UI controls, not real authorization/durable message delivery proof.
// Owner contract: GET owner/external-agents -> {agents:[{key,name,address,enabled,created}]}
// (at most 100); POST {key,name,days:30|90,authority_confirmed:true} -> 201
// {agent,token:{id,...},plaintext}. POST owner/external-agents/tokens uses the
// same body/response, labels a new credential and preserves Agent name/address.
// Old tokens are not automatically revoked. created/expiry/last_used are epoch seconds.
// Keys are immutable ASCII segments (1..64); names <=128 UTF-8 bytes. Returned
// address is AppID/key, displayed verbatim, never guessed as a URL. Registry
// tokens retain gateway authority under current Policy plus only their own inbox.
// No create retry: an uncertain response requires reading registry/token metadata;
// an admitted key can be found, but its plaintext cannot be recovered by reads.
// Run with EXTERNAL_PLAYWRIGHT_MODULE pointing at an installed Playwright module.
// The fixture owns one random loopback listener and closes its own browser/server.

import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.EXTERNAL_PLAYWRIGHT_MODULE));
const prefix='/api/workspaces/ws_fixture/apps/app_fixture/ui/';
const identity={workspace:'ws_fixture',app:'app_fixture',agent:'app_fixture'};
const secret='ext1_'+'a'.repeat(32)+'.'+'b'.repeat(64);
let records=[],tokens=[],scenario='normal',posts=[],registryGets=0,clipboard=[];
const row=(key,name)=>({key,name,address:'app_fixture/'+key,enabled:true,created:1792724400});
const server=createServer(async(req,res)=>{
 res.setHeader('cache-control','no-store');res.setHeader('content-type','application/json');
 if(!req.url.startsWith(prefix)){res.writeHead(404).end();return;}
 const path=req.url.slice(prefix.length);
 if(path==='owner/state'){res.end(JSON.stringify({identity,generation:'g',enabled:true,activation_available:true,tokens}));return;}
 if(path==='owner/integration'){res.writeHead(503).end('{}');return;}
 if(path==='owner/external-agents'||path==='owner/external-agents/tokens'){
  if(req.method==='POST'){
   let b='';for await(const chunk of req)b+=chunk;
   const body=JSON.parse(b);posts.push({body,csrf:req.headers['x-external-agent-csrf']});
   if(scenario==='refused'){res.writeHead(400).end('{}');return;}
   const replacement=path.endsWith('/tokens');const agent=replacement?records.find(agent=>agent.key===body.key):row(body.key,body.name);if(!replacement)records.push(agent);tokens.push({id:'tok_child',name:'external/'+body.key,expires:Date.now()/1000+3600,last_used:null,revoked:false});
   if(scenario==='lost'){res.writeHead(503).end('{}');return;}
   res.writeHead(201).end(JSON.stringify({agent,token:{id:'tok_child'},plaintext:secret}));return;
  }
  registryGets++;
  if(scenario==='denied'){res.writeHead(401).end('{}');return;}
  if(scenario==='offline'){res.writeHead(503).end('{}');return;}
  if(scenario==='oversize'){res.end(' '.repeat(131073));return;}
  res.end(JSON.stringify({agents:scenario==='foreign'?[{...row('x','foreign'),address:'another_app/x'}]:scenario==='excess'?Array.from({length:101},(_,i)=>row('k'+i,'name')):records}));return;
 }
 if(req.method!=='GET'){res.writeHead(405).end();return;}
 const files={'':'settings.html','settings.js':'settings.js','settings.css':'settings.css'};
 if(!files[path]){res.writeHead(404).end();return;}
 res.setHeader('content-type',path.endsWith('.js')?'text/javascript':path.endsWith('.css')?'text/css':'text/html');res.end(await readFile(new URL('../src/runtime/'+files[path],import.meta.url)));
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser=await chromium.launch({headless:true});let checks=0;
try{
 for(const [width,locale,colorScheme] of [[1100,'en','light'],[390,'zh','dark']]){
  records=[];tokens=[];scenario='normal';const before=posts.length;
  const context=await browser.newContext({viewport:{width,height:844},locale,colorScheme});await context.addInitScript(()=>{globalThis.addressCopies=[];Object.defineProperty(navigator,'clipboard',{value:{writeText:async text=>addressCopies.push(text)},configurable:true});});
  const page=await context.newPage();const errors=[];page.on('pageerror',()=>errors.push('pageerror'));
  await page.goto(`http://127.0.0.1:${server.address().port}${prefix}`);await page.locator('#controls').waitFor();await page.waitForFunction(()=>!document.getElementById('external-agents-refresh').disabled);
  assert.ok((await page.locator('#external-agents-status').textContent()).length>0);checks++;
  await page.locator('#external-agent-key').fill('echo_1');await page.locator('#external-agent-name').fill('中'.repeat(65));await page.locator('#external-agent-authority').check();await page.locator('#external-agent-create-button').click();
  assert.equal(posts.length,before);assert.ok((await page.locator('#external-agent-feedback').textContent()).length>0);checks++;
  const name='<img src=x onerror=alert(1)> display';await page.locator('#external-agent-name').fill(name);await page.selectOption('#external-agent-days','90');await page.locator('#external-agent-create-button').focus();await page.keyboard.press('Enter');
  await page.waitForFunction(()=>!document.getElementById('external-agent-create-button').disabled);
  assert.equal(posts.length,before+1);assert.deepEqual(posts.at(-1),{body:{key:'echo_1',name,days:90,authority_confirmed:true},csrf:'1'});assert.equal(await page.locator('#secret').inputValue(),secret);assert.ok(/workspace|工作区/.test(await page.locator('#secret-label').textContent()));
  assert.equal(await page.locator('#external-agents-list img').count(),0);assert.ok((await page.locator('#external-agents-list').textContent()).includes(name));assert.equal(await page.locator('.external-agent-address').inputValue(),'app_fixture/echo_1');assert.ok((await page.locator('#tokens').textContent()).includes('tok_child'));assert.ok(!(await page.locator('#tokens').textContent()).includes('1970'));checks++;
  await page.locator('#external-agents-list button').first().click();assert.deepEqual(await page.evaluate(()=>addressCopies),['app_fixture/echo_1']);assert.ok(!(await page.locator('#external-agents-list').textContent()).includes(secret));checks++;
  await page.evaluate(()=>{navigator.clipboard.writeText=async()=>{throw Error('denied');};});await page.locator('#external-agents-list button').first().click();assert.equal(await page.locator('.external-agent-address').evaluate(node=>node.selectionEnd-node.selectionStart),'app_fixture/echo_1'.length);checks++;
  await page.locator('#dismiss').click();assert.equal(await page.locator('#secret').inputValue(),'');assert.equal(await page.locator('#secret-area').isVisible(),false);assert.equal(await page.locator('#external-agent-create-button').evaluate(node=>node===document.activeElement),true);checks++;
  await page.locator('#external-agents-refresh').click();await page.waitForFunction(()=>!document.getElementById('external-agents-refresh').disabled);assert.equal(await page.locator('#secret').inputValue(),'');checks++;
  scenario='lost';await page.locator('#external-agent-key').fill('lost_response');await page.locator('#external-agent-name').fill('Unknown outcome');await page.locator('#external-agent-authority').check();await page.locator('#external-agent-create-button').click();await page.waitForFunction(()=>document.getElementById('external-agent-feedback').textContent.length>0);
  assert.equal(posts.length,before+2);assert.equal(await page.locator('#external-agent-create-button').isDisabled(),true);assert.equal(await page.locator('#secret').inputValue(),'');await page.waitForTimeout(50);assert.equal(posts.length,before+2);checks++;
  scenario='normal';await page.locator('#external-agents-refresh').click();await page.waitForFunction(()=>!document.getElementById('external-agents-refresh').disabled);assert.equal(await page.locator('.external-agent-address').count(),2);assert.equal(await page.locator('#secret').inputValue(),'');assert.equal(await page.locator('#external-agent-key').inputValue(),'');assert.ok(/cannot be recovered|无法恢复/.test(await page.locator('#external-agent-feedback').textContent()));checks++;
  await page.locator('.external-agent-replace').first().click();assert.equal(await page.locator('#external-agent-key').inputValue(),'echo_1');assert.equal(await page.locator('#external-agent-key').getAttribute('readonly'),'');assert.ok(/Old tokens|旧 token/.test(await page.locator('#external-agent-token-note').textContent()));
  await page.locator('#external-agent-name').fill('Replacement credential');await page.locator('#external-agent-authority').check();const replacementRequest=page.waitForRequest(req=>req.url().endsWith('/owner/external-agents/tokens')&&req.method()==='POST');await page.locator('#external-agent-create-button').click();await replacementRequest;await page.waitForFunction(()=>!document.getElementById('external-agent-create-button').disabled);
  assert.equal(posts.length,before+3);assert.deepEqual(posts.at(-1).body,{key:'echo_1',name:'Replacement credential',days:90,authority_confirmed:true});assert.equal(records.length,2);assert.equal(records[0].name,name);assert.equal(tokens.length,3);assert.ok(tokens.every(token=>!token.revoked));assert.equal(await page.locator('#secret').inputValue(),secret);assert.ok(/not been revoked|未被撤销/.test(await page.locator('#external-agent-feedback').textContent()));checks++;
  await page.locator('#dismiss').click();await page.locator('.external-agent-replace').first().click();await page.locator('#external-agent-cancel').click();assert.equal(await page.locator('#external-agent-key').getAttribute('readonly'),null);assert.equal(await page.locator('#external-agent-key').inputValue(),'');checks++;
  scenario='lost';await page.locator('.external-agent-replace').first().click();await page.locator('#external-agent-name').fill('Lost replacement');await page.locator('#external-agent-authority').check();await page.locator('#external-agent-create-button').click();await page.waitForFunction(()=>document.getElementById('external-agent-create-button').disabled&&document.getElementById('external-agent-feedback').textContent.length>0);assert.equal(posts.length,before+4);assert.equal(await page.locator('#secret').inputValue(),'');
  scenario='normal';await page.locator('#external-agents-refresh').click();await page.waitForFunction(()=>!document.getElementById('external-agents-refresh').disabled);assert.ok(/cannot confirm|不能确认/.test(await page.locator('#external-agent-feedback').textContent()));assert.equal(records.length,2);assert.equal(posts.length,before+4);await page.locator('#external-agent-cancel').click();checks++;
  for(const kind of ['denied','offline','foreign','excess','oversize']){
   scenario=kind;await page.locator('#external-agents-refresh').click();await page.waitForFunction(()=>!document.getElementById('external-agents-refresh').disabled);assert.equal(await page.locator('#external-agents-list .external-agent-row').count(),0);assert.ok((await page.locator('#external-agents-status').textContent()).length>0);checks++;
  }
  scenario='normal';await page.locator('#external-agents-refresh').click();await page.waitForFunction(()=>!document.getElementById('external-agents-refresh').disabled);
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
  for(const id of ['external-agents-refresh','external-agent-create-button','external-agent-days'])assert.ok((await page.locator('#'+id).boundingBox()).height>=44);
  await page.selectOption('#language',locale==='zh'?'en':'zh');assert.ok((await page.locator('#external-agents-title').textContent()).includes(locale==='zh'?'External':'外部'));assert.equal(await page.locator('.external-agent-address').count(),2);checks++;
  if(width===390&&process.env.REGISTRY_SCREENSHOT)await page.screenshot({path:process.env.REGISTRY_SCREENSHOT,fullPage:true});
  await page.evaluate(()=>dispatchEvent(new Event('pagehide')));assert.equal(await page.locator('#secret').inputValue(),'');assert.equal(await page.locator('#external-agents-list .external-agent-row').count(),0);assert.deepEqual(errors,[]);checks++;
  await context.close();
 }
 assert.equal(posts.length,8);console.log(`PASS ${checks} registry controls, 2 layouts/languages, 8 explicit POSTs only; ambiguity/no retry/once-only secret/addresses/denied/malformed/prefix/keyboard. Synthetic HTTP only.`);
}finally{await browser.close();await new Promise(resolve=>server.close(resolve));}
