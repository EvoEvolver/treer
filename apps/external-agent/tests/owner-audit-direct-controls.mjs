// Synthetic HTTP controls for the caller-owned leaf; no authentic owner/Core claim.
import assert from 'node:assert/strict';import {createHash} from 'node:crypto';
import {createServer} from 'node:http';import {readFile} from 'node:fs/promises';import {pathToFileURL} from 'node:url';
import {runOwnerAudit,validateAuditBinding} from './owner-audit-direct.mjs';
const {chromium}=await import(pathToFileURL(process.env.EXTERNAL_PLAYWRIGHT_MODULE));
let scenario='records',mutations=0,auditRequests=0;
const identity={workspace:'ws_fixture',app:'app_fixture',service:'svc_fixture',agent:'ag_fixture'};
const row={request_id:'allowed_request',token:'masked_token_id_12345678',operation:'transcript',target:'ag_allowed',started:'2026-10-22T00:00:00Z',ended:null,outcome:'completed'};
const server=createServer(async(req,res)=>{
 req.url=req.url.replace('/api/workspaces/ws_fixture/apps/app_fixture/ui','');res.setHeader('cache-control','no-store');
 if(req.method!=='GET'){mutations++;res.writeHead(405).end();return;}
 if(req.url==='/owner/state'){res.setHeader('content-type','application/json');res.end(JSON.stringify({identity,tokens:[],generation:'g',enabled:false,activation_available:false}));return;}
 if(req.url==='/owner/audit'){
  auditRequests++;res.setHeader('content-type','application/json');
  if(scenario==='denied'){res.writeHead(403).end('{}');return;}
  if(scenario==='offline'){res.writeHead(503).end('{}');return;}
  if(scenario==='oversize'){res.end(' '.repeat(131073));return;}
  res.end(JSON.stringify({identity:scenario==='foreign'?{...identity,app:'foreign'}:identity,requests:scenario==='empty'?[]:scenario==='excess'?Array(101).fill(row):[row,{...row,request_id:'denied_request',target:'ag_denied',outcome:'policy_denied'}]}));return;
 }
 const files={'/':'settings.html','/settings.js':'settings.js','/settings.css':'settings.css'};
 if(!files[req.url]){res.writeHead(404).end();return;}
 res.setHeader('content-type',req.url.endsWith('.js')?'application/javascript':req.url.endsWith('.css')?'text/css':'text/html');
 res.end(await readFile(new URL('../src/runtime/'+files[req.url],import.meta.url)));
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser=await chromium.launch({headless:true});
try{
 const ui_sha256={};for(const name of ['settings.js','settings.css'])ui_sha256[name]=createHash('sha256').update(await readFile(new URL('../src/runtime/'+name,import.meta.url))).digest('hex');
 const binding={generation:'g',ui_sha256,owner_url:`http://127.0.0.1:${server.address().port}/api/workspaces/ws_fixture/apps/app_fixture/ui/`,identity,source_sha:'b'.repeat(40),artifact_sha256:'c'.repeat(64),deadline_ms:Date.now()+120000,expected_records:[{request_id:'allowed_request',operation:'transcript',target:'ag_allowed',outcome:'completed'},{request_id:'denied_request',operation:'transcript',target:'ag_denied',outcome:'policy_denied'}]};
 for(const change of [b=>b.owner_url='https://evil.test/',b=>b.identity.app='other',b=>b.deadline_ms=0,b=>b.expected_records=[]]){const b=structuredClone(binding);change(b);assert.throws(()=>validateAuditBinding(b));}
 for(const kind of ['records','denied','foreign','oversize','wrong_generation','wrong_asset']){
  scenario=kind;const context=await browser.newContext();const page=await context.newPage();
  const b=structuredClone(binding);if(kind==='wrong_generation')b.generation='other';if(kind==='wrong_asset')b.ui_sha256['settings.js']='0'.repeat(64);const result=await runOwnerAudit({page,binding:b});assert.equal(result.passed,kind==='records');assert.equal(result.api_mocking,false);assert.equal(result.forbidden_count,0);if(kind==='records')assert.equal(result.responses.length,2);
  assert.equal(page.isClosed(),false);await context.close();
 }
 assert.equal(mutations,0);console.log('PASS direct-leaf embedded prefix/2 layouts/correlated records +5 failures +4 invalid bindings; synthetic only');
}finally{await browser.close();await new Promise(resolve=>server.close(resolve));}
