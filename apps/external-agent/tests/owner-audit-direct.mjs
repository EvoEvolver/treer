// Caller-owned authentic browser/context. This module creates no identity or stack.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
export function validateAuditBinding(b){
 const url=new URL(b.owner_url);
 assert.ok(!url.username&&!url.password&&!url.search&&!url.hash);
 assert.ok(url.protocol==='https:'||(url.protocol==='http:'&&['127.0.0.1','[::1]'].includes(url.hostname)));
 for(const key of ['workspace','app','service','agent'])assert.match(b.identity[key],/^[A-Za-z0-9_-]{1,128}$/);
 assert.equal(url.pathname,`/api/workspaces/${b.identity.workspace}/apps/${b.identity.app}/ui/`);
 assert.match(b.generation,/^[A-Za-z0-9_-]{1,128}$/);
 for(const name of ['settings.js','settings.css'])assert.match(b.ui_sha256[name],/^[a-f0-9]{64}$/);
 assert.match(b.source_sha,/^[a-f0-9]{40}$/);assert.match(b.artifact_sha256,/^[a-f0-9]{64}$/);
 assert.ok(Number.isFinite(b.deadline_ms)&&b.deadline_ms>Date.now()&&b.deadline_ms<=Date.now()+300000);
 assert.ok(Array.isArray(b.expected_records)&&b.expected_records.length===2);
 assert.deepEqual(new Set(b.expected_records.map(r=>r.outcome)),new Set(['completed','policy_denied']));
 for(const row of b.expected_records){for(const k of ['request_id','operation','target'])assert.ok(typeof row[k]==='string'&&row[k].length>0&&row[k].length<=256);}
 assert.notEqual(b.expected_records[0].request_id,b.expected_records[1].request_id);
 return url;
}
export async function runOwnerAudit({page,binding}){
 let stage='binding',forbidden=0,installed=false;const responseHashes=[],servedUiHashes={};
 const guard=async route=>{const req=route.request(),url=new URL(req.url());if(Date.now()<binding.deadline_ms&&url.origin===base.origin&&url.pathname.startsWith(base.pathname)&&['GET','HEAD','OPTIONS'].includes(req.method()))return route.continue();forbidden++;return route.abort();};
 let base;
 try{
  base=validateAuditBinding(binding);
  assert.equal(typeof page.route,'function');
  stage='transport';await page.route('**/*',guard);installed=true;
  stage='owner_page';
  const observe=name=>page.waitForResponse(r=>r.url()===new URL(name,base).href&&r.request().method()==='GET',{timeout:remaining()});
  const assets=['settings.js','settings.css'].map(name=>({name,promise:observe(name)}));
  const statePromise=observe('owner/state');
  // Consume rejection immediately if navigation fails; preserve fixed-stage failure.
  for(const item of assets)item.promise.catch(()=>{});statePromise.catch(()=>{});
  await page.goto(base.href,{waitUntil:'domcontentloaded',timeout:remaining()});
  stage='served_ui';
  for(const {name,promise} of assets){const response=await promise;assert.equal(response.status(),200);const bytes=await response.body();assert.ok(bytes.length<=131072);const hash=createHash('sha256').update(bytes).digest('hex');assert.equal(hash,binding.ui_sha256[name]);servedUiHashes[name]=hash;}
  stage='generation';const actualState=await statePromise;assert.equal(actualState.status(),200);assert.equal(actualState.headers()['cache-control'],'no-store');const stateBytes=await actualState.body();assert.ok(stateBytes.length<=131072);const state=JSON.parse(stateBytes.toString());assert.equal(state.generation,binding.generation);assert.equal(state.enabled,false);for(const key of ['workspace','app','agent'])assert.equal(state.identity[key],binding.identity[key]);
  await page.locator('#controls').waitFor({state:'visible',timeout:remaining()});
  const identity=await page.locator('#identity').textContent();
  assert.equal(identity,`${binding.identity.workspace} · ${binding.identity.app} · ${binding.identity.agent}`);
  for(const width of [1100,390]){
   stage=`refresh_${width}`;await page.setViewportSize({width,height:844});
   const responsePromise=page.waitForResponse(r=>r.url()===new URL('owner/audit',base).href&&r.request().method()==='GET',{timeout:remaining()});
   await page.locator('#audit-refresh').click({timeout:remaining()});
   const response=await responsePromise;assert.equal(response.status(),200);assert.equal(response.headers()['cache-control'],'no-store');
   const bytes=await response.body();assert.ok(bytes.length<=131072);const data=JSON.parse(bytes.toString());
   for(const key of ['workspace','app','service','agent'])assert.equal(data.identity[key],binding.identity[key]);
   assert.ok(Array.isArray(data.requests)&&data.requests.length<=100);
   for(const expected of binding.expected_records){const rows=data.requests.filter(r=>r.request_id===expected.request_id);assert.equal(rows.length,1);for(const key of ['operation','target','outcome'])assert.equal(rows[0][key],expected[key]);}
   stage=`render_${width}`;await page.waitForFunction(()=>!document.getElementById('audit-refresh').disabled,{},{timeout:remaining()});
   assert.equal(await page.locator('#audit .token').count(),data.requests.length);
   for(const expected of binding.expected_records){const row=page.locator('#audit .token').filter({hasText:expected.request_id});assert.equal(await row.count(),1);const text=await row.textContent();assert.ok(text.includes(expected.operation)&&text.includes(expected.outcome)&&text.includes(expected.target));}
   const rendered=await page.locator('#audit').textContent();
   for(const row of data.requests){assert.ok(rendered.includes(`…${row.token.slice(-8)}`));if(row.token.length>8)assert.ok(!rendered.includes(row.token));}
   assert.equal(await page.locator('#audit input,#audit textarea,#audit button,#audit a,#audit img').count(),0);
   assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
   responseHashes.push({width,rows:data.requests.length,sha256:createHash('sha256').update(bytes).digest('hex')});
  }
  assert.equal(forbidden,0);assert.ok(Date.now()<binding.deadline_ms);stage='complete';
 }catch{/* Fixed stage only: never return private DOM/network error strings. */}
 finally{if(installed)await page.unroute('**/*',guard);}
 return {passed:stage==='complete',stage,source_sha:binding?.source_sha,artifact_sha256:binding?.artifact_sha256,generation:binding?.generation,served_ui_sha256:servedUiHashes,responses:responseHashes,forbidden_count:forbidden,api_mocking:false,identity_owner:'caller',session_cleanup_owner:'caller',browser_closed:false};
 function remaining(){const ms=binding.deadline_ms-Date.now();assert.ok(ms>0);return Math.min(ms,15000);}
}
