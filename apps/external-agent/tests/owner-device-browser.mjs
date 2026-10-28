// Synthetic owner HTTP UI controls; no real owner authorization or token claim proof.
// POST owner/device/lookup {user_code} -> {request:{key,name,expires_at(epoch s),
// status,scope,existing_agent,existing_name}}. decision {user_code,approve,days,
// authority_confirmed} -> {status}. No device_code/access_token enters this UI.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.EXTERNAL_PLAYWRIGHT_MODULE));
const prefix='/api/workspaces/ws_fixture/apps/app_fixture/ui/';
let scenario='pending',decisions=[],lookups=[];
const identity={workspace:'ws_fixture',app:'app_fixture',agent:'app_fixture'};
const server=createServer(async(req,res)=>{
 res.setHeader('cache-control','no-store');res.setHeader('content-type','application/json');
 if(!req.url.startsWith(prefix)){res.writeHead(404).end();return;}const path=req.url.slice(prefix.length);
 if(path==='owner/state'){res.end(JSON.stringify({identity,generation:'g',enabled:true,activation_available:true,tokens:[]}));return;}
 if(path==='owner/external-agents'){res.end('{"agents":[]}');return;}
 if(path==='owner/integration'){res.writeHead(503).end('{}');return;}
 if(path.startsWith('owner/device/')){
  let text='';for await(const chunk of req)text+=chunk;const body=JSON.parse(text);assert.equal(req.headers['x-external-agent-csrf'],'1');assert.equal(req.method,'POST');
  if(path.endsWith('/decision')){decisions.push(body);if(scenario==='lost'){res.writeHead(503).end('{}');return;}res.end(JSON.stringify({status:body.approve?'approved':'denied'}));return;}
  lookups.push(body);if(scenario==='limited'){res.writeHead(429).end('{}');return;}if(scenario==='auth'){res.writeHead(401).end('{}');return;}
  if(scenario==='oversize'){res.end(' '.repeat(16385));return;}
  if(scenario==='missing'){res.writeHead(404).end('{}');return;}
  const request={key:'echo_1',name:'<img src=x onerror=alert(1)>',expires_at:Math.floor(Date.now()/1000)+(scenario==='expired'?-1:scenario==='expireSoon'?1:600),status:['approved','denied','claimed'].includes(scenario)?scenario:'pending',scope:scenario==='scope'?'readonly':'workspace_operations_and_own_inbox',existing_agent:true,existing_name:'Existing Agent'};
  res.end(JSON.stringify({request}));return;
 }
 const file={'':'settings.html','settings.js':'settings.js','settings.css':'settings.css'}[path];if(!file){res.writeHead(404).end();return;}res.setHeader('content-type',path.endsWith('.js')?'text/javascript':path.endsWith('.css')?'text/css':'text/html');res.end(await readFile(new URL('../src/runtime/'+file,import.meta.url)));
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const browser=await chromium.launch();let checks=0;
try{for(const [width,locale] of [[1100,'en'],[390,'zh']]){
 scenario='pending';const context=await browser.newContext({viewport:{width,height:844},locale});const page=await context.newPage();const errors=[];page.on('pageerror',()=>errors.push('pageerror'));await page.goto(`http://127.0.0.1:${server.address().port}${prefix}`);await page.waitForFunction(()=>!document.getElementById('device-lookup-button').disabled);
 const lookup=async(kind)=>{scenario=kind;await page.locator('#device-code').fill('abcd-ef12');await page.locator('#device-lookup-button').click();await page.waitForFunction(()=>!document.getElementById('device-lookup-button').disabled);};
 await page.locator('#device-code').fill('WXYZ-1234');const invalidCount=lookups.length;await page.locator('#device-lookup-button').click();assert.equal(lookups.length,invalidCount);assert.ok((await page.locator('#device-status').textContent()).length>0);checks++;
 await lookup('pending');assert.deepEqual(lookups.at(-1),{user_code:'ABCD-EF12'});assert.equal(await page.locator('#device-details img').count(),0);assert.ok((await page.locator('#device-details').textContent()).includes('Existing Agent'));assert.ok(/workspace|工作区/.test(await page.locator('#device-details').textContent()));assert.equal(await page.locator('#device-approve').isDisabled(),true);checks++;
 await page.locator('#device-authority').check();await page.selectOption('#device-days','90');await page.locator('#device-approve').focus();await page.keyboard.press('Enter');await page.waitForFunction(()=>!document.getElementById('device-lookup-button').disabled);assert.deepEqual(decisions.at(-1),{user_code:'ABCD-EF12',approve:true,days:90,authority_confirmed:true});assert.equal(await page.locator('#device-request').isVisible(),false);assert.equal(await page.locator('#secret').inputValue(),'');checks++;
 await lookup('pending');await page.locator('#device-deny').click();await page.waitForFunction(()=>!document.getElementById('device-lookup-button').disabled);assert.equal(decisions.at(-1).approve,false);assert.equal(decisions.at(-1).authority_confirmed,false);checks++;
 for(const kind of ['approved','denied','claimed','expired','auth','scope','oversize','missing','limited']){await lookup(kind);assert.equal(await page.locator('#device-approve').isDisabled(),true);assert.ok((await page.locator('#device-status').textContent()).length>0);checks++;}
 await lookup('pending');await page.locator('#device-authority').check();await page.locator('#device-code').fill('WXYZ-1234');assert.equal(await page.locator('#device-request').isVisible(),false);assert.equal(await page.locator('#device-approve').isDisabled(),true);checks++;
 await lookup('expireSoon');await page.locator('#device-authority').check();await page.waitForFunction(()=>document.getElementById('device-approve').disabled);assert.ok(/expired|过期/.test(await page.locator('#device-status').textContent()));checks++;
 await lookup('pending');scenario='lost';await page.locator('#device-authority').check();const count=decisions.length;await page.locator('#device-approve').click();await page.waitForFunction(()=>!document.getElementById('device-lookup-button').disabled);assert.equal(decisions.length,count+1);assert.equal(await page.locator('#device-approve').isDisabled(),true);assert.ok(/not be confirmed|未确认/.test(await page.locator('#device-status').textContent()));await page.waitForTimeout(50);assert.equal(decisions.length,count+1);checks++;
 await lookup('pending');await page.selectOption('#language',locale==='zh'?'en':'zh');assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);for(const id of ['device-lookup-button','device-approve','device-deny','device-days'])assert.ok((await page.locator('#'+id).boundingBox()).height>=44);checks++;
 await page.evaluate(()=>dispatchEvent(new Event('pagehide')));assert.equal(await page.locator('#device-request').isVisible(),false);assert.equal(await page.locator('#device-code').inputValue(),'');assert.deepEqual(errors,[]);checks++;
 await context.close();
}assert.equal(decisions.length,6);console.log(`PASS ${checks} device owner UI controls, 2 layouts/languages, 6 explicit decisions; synthetic HTTP only.`);
}finally{await browser.close();await new Promise(resolve=>server.close(resolve));}
