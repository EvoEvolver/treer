// Focused UI HTTP fixture, not actual Core/owner authorization or deployed acceptance.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
const { chromium }=await import(pathToFileURL(process.env.EXTERNAL_PLAYWRIGHT_MODULE));
let scenario='records',mutations=0,auditRequests=0;
const identity={workspace:'ws_fixture',app:'app_fixture',agent:'ag_fixture'};
const row={request_id:'<img src=x onerror=alert(1)>',token:'TOKEN_ID_NEVER_SHOW_FULL_12345678',operation:'transcript',target:'ag_target',started:'2026-10-22T00:00:00Z',ended:null,outcome:'outcome_unknown'};
const server=createServer(async(req,res)=>{
 res.setHeader('cache-control','no-store');
 if(req.method!=='GET'){mutations++;res.writeHead(405).end();return;}
 if(req.url==='/owner/state'){res.setHeader('content-type','application/json');res.end(JSON.stringify({identity,tokens:[],generation:'g',enabled:false,activation_available:false}));return;}
 if(req.url==='/owner/audit'){
  auditRequests++;res.setHeader('content-type','application/json');
  if(scenario==='denied'){res.writeHead(403).end('{}');return;}
  if(scenario==='offline'){res.writeHead(503).end('{}');return;}
  if(scenario==='oversize'){res.end(' '.repeat(131073));return;}
  res.end(JSON.stringify({identity:scenario==='foreign'?{...identity,app:'foreign'}:identity,requests:scenario==='empty'?[]:scenario==='excess'?Array(101).fill(row):[row]}));return;
 }
 const files={'/':'settings.html','/settings.js':'settings.js','/settings.css':'settings.css'};
 if(!files[req.url]){res.writeHead(404).end();return;}
 res.setHeader('content-type',req.url.endsWith('.js')?'application/javascript':req.url.endsWith('.css')?'text/css':'text/html');
 res.end(await readFile(new URL('../src/runtime/'+files[req.url],import.meta.url)));
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser=await chromium.launch({headless:true});
try{
 for(const [width,locale,scheme] of [[1100,'en','light'],[390,'zh','dark']]){
  const context=await browser.newContext({viewport:{width,height:844},locale,colorScheme:scheme});
  const page=await context.newPage();await page.goto(`http://127.0.0.1:${server.address().port}/`);await page.locator('#controls').waitFor();
  for(const kind of ['records','empty','denied','offline','foreign','excess','oversize']){
   scenario=kind;await page.locator('#audit-refresh').click();
   await page.waitForFunction(()=>!document.getElementById('audit-refresh').disabled);
   if(kind==='records'){
    assert.ok((await page.locator('#audit').textContent()).includes('outcome_unknown'));
    assert.ok((await page.locator('#audit').textContent()).includes('…12345678'));
    assert.ok(!(await page.locator('#audit').textContent()).includes('TOKEN_ID_NEVER_SHOW_FULL'));
    assert.equal(await page.locator('#audit img').count(),0);
   }else {assert.equal(await page.locator('#audit .token').count(),0);assert.ok((await page.locator('#audit-status').textContent()).length>0);}
  }
  scenario='records';await page.locator('#audit-refresh').click();await page.waitForFunction(()=>!document.getElementById('audit-refresh').disabled);
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
  if(width===390&&process.env.AUDIT_SCREENSHOT)await page.screenshot({path:process.env.AUDIT_SCREENSHOT,fullPage:true});
  await page.selectOption('#language',locale==='zh'?'en':'zh');assert.equal(await page.locator('#audit .token').count(),0);
  await context.close();
 }
 assert.equal(mutations,0);assert.equal(auditRequests,16);
 console.log('PASS 2 layouts x 7 audit states + language reset, masked IDs/XSS/no mutation; synthetic HTTP UI fixture only');
}finally{await browser.close();await new Promise(resolve=>server.close(resolve));}
