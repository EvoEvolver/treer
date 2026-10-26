#!/usr/bin/env node
// Closed UI/HTTP controls only. No real member, OAuth service, credentials or send.
import {createServer} from 'node:http';
import {readFileSync,mkdirSync} from 'node:fs';
import {createRequire} from 'node:module';
import {resolve,join} from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const [modulePath,screenshots]=process.argv.slice(2);
if(!screenshots)throw Error('usage: email-recovery-browser.mjs PLAYWRIGHT_MODULE SCREENSHOTS');
const {chromium}=createRequire(import.meta.url)(resolve(modulePath));
const dist=fileURLToPath(new URL('../dist/',import.meta.url));
const thread='a'.repeat(64),prefix='/api/workspaces/ws/apps/app/ui/';
let failure={status:403,code:'email_reply_unavailable'},readable=false,postFailure=false,posts=[],authHits=0,logoutHits=0,origin;
const server=createServer(async(req,res)=>{
 const u=new URL(req.url,origin);let path=u.pathname;if(path.startsWith(prefix))path='/'+path.slice(prefix.length);
 const json=(status,v)=>{res.writeHead(status,{'content-type':'application/json','cache-control':'no-store'});res.end(JSON.stringify(v));};
 if(path==='/api/endpoint/auth/config')return json(200,{settings_url:origin+'/',login_url:origin+'/api/email/auth/start'});
 if(path.startsWith('/api/email/auth/start')){authHits++;return json(200,{synthetic:true});}
 if(path==='/api/email/auth/logout'){logoutHits++;return json(200,{});}
 if(path==='/api/email/thread/'+thread){
  if(req.method==='POST'){let data='';for await(const chunk of req)data+=chunk;posts.push(JSON.parse(data));if(postFailure){res.writeHead(403,{'content-type':'application/json'});return res.end(JSON.stringify({error:{code:'owner_csrf_rejected',message:'private-do-not-render'}}));}return json(202,{});}
  if(readable)return json(200,{thread:{id:thread,endpoint_id:'ep-fixture',recipient_agent_id:'agent-fixture',messages:[{source_id:'one',body:'Synthetic conversation',member_id:null,created_at:1700000000}]}});
  if(failure.code==='non-json'){res.writeHead(failure.status,{'content-type':'text/html'});return res.end('<b>private-do-not-render</b>');}
  return json(failure.status,{error:{code:failure.code,message:'private-do-not-render',expected_member:'do-not-render'}});
 }
 const file=path==='/'?'index.html':path==='/assets/app.js'?'app.js':path==='/assets/app.css'?'app.css':null;
 if(file){res.writeHead(200,{'content-type':file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html'});return res.end(readFileSync(join(dist,file)));}
 res.writeHead(404);res.end();
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));origin='http://127.0.0.1:'+server.address().port;
mkdirSync(screenshots,{recursive:true,mode:0o700});let browser;let checks=0;
try{
 browser=await chromium.launch({headless:true});
 const cases=[
  [401,'owner_auth_required','Sign in with Treer',true],
  [401,'owner_identity_invalid','Sign in with Treer',true],
  [403,'email_reply_unavailable','Conversation access unavailable',true],
  [403,'current_workspace_member_required','Workspace member identity required',true],
  [403,'current_human_owner_required','Workspace member identity required',true],
  [403,'owner_csrf_rejected','Request context rejected',false],
  [403,'email_request_host_rejected','Request context rejected',false],
  [403,'email_request_context_rejected','Request context rejected',false],
  [403,'unrecognized-private-code','Conversation access unavailable',true],
  [403,'non-json','Conversation access unavailable',true],
  [503,'email_reply_unavailable','Request unavailable',false],
 ];
 for(const width of [1280,390])for(const [status,code,title,reauth]of cases){
  failure={status,code};readable=false;
  const page=await browser.newPage({viewport:{width,height:900},locale:'en-US',colorScheme:width===390?'dark':'light'});
  await page.goto(origin+'/#email-thread='+thread);
  await page.getByRole('heading',{name:title,exact:true}).waitFor();
  const link=page.getByRole('link',{name:'Re-authenticate for this conversation',exact:true});
  if(reauth){await link.waitFor();assert.equal(await link.getAttribute('href'),origin+'/api/email/auth/start?thread='+thread+'&reauth=1');}else assert.equal(await link.count(),0);
  assert.equal(await page.getByRole('textbox').count(),0);
  assert(!await page.locator('body').innerText().then(t=>t.includes('private-do-not-render')||t.includes('unrecognized-private-code')||t.includes('expected_member')));
  assert(await page.locator('body').evaluate(e=>e.scrollWidth<=innerWidth));
  if(width===390&&status===403&&code==='email_reply_unavailable')await page.screenshot({path:join(screenshots,'generic-390-dark.png'),fullPage:true});
  await page.close();checks++;
 }
 assert.equal(authHits,0);assert.equal(logoutHits,0);assert.equal(posts.length,0);
 // Embedded surface keeps all requests relative and reauth opens configured root,
 // not an injected identity or an embedded mutation.
 failure={status:403,code:'current_workspace_member_required'};
 const embed=await browser.newPage();await embed.goto(origin+prefix+'#email-thread='+thread);
 const relog=embed.getByRole('link',{name:'Re-authenticate for this conversation',exact:true});await relog.waitFor();assert.equal(await relog.getAttribute('target'),'_blank');assert.equal(await relog.getAttribute('rel'),'noopener noreferrer');
 assert.equal(await relog.getAttribute('href'),origin+'/api/email/auth/start?thread='+thread+'&reauth=1');await embed.close();checks++;
 const zh=await browser.newPage({locale:'zh-CN',viewport:{width:390,height:900},colorScheme:'dark'});await zh.goto(origin+'/#email-thread='+thread);const zhLink=zh.getByRole('link',{name:'重新认证此会话',exact:true});await zhLink.waitFor();await zhLink.focus();assert(await zhLink.evaluate(e=>e===document.activeElement));assert(await zh.locator('body').evaluate(e=>e.scrollWidth<=innerWidth));await zh.screenshot({path:join(screenshots,'member-390-zh-dark.png'),fullPage:true});await zh.close();checks++;
 // Explicit recovery click only, exact thread/reauth query; never logout.
 const recovery=await browser.newPage();await recovery.goto(origin+'/#email-thread='+thread);await recovery.getByRole('link',{name:'Re-authenticate for this conversation',exact:true}).click();await recovery.waitForURL('**/api/email/auth/start?thread='+thread+'&reauth=1');assert.equal(authHits,1);assert.equal(logoutHits,0);await recovery.close();checks++;
 // Reply denial keeps immutable body/key and never automatically retries.
 readable=true;postFailure=true;
 const reply=await browser.newPage();await reply.goto(origin+'/#email-thread='+thread);await reply.getByRole('textbox',{name:'Message',exact:true}).fill('Synthetic reply');await reply.getByRole('button',{name:'Queue reply',exact:true}).click();await reply.getByRole('heading',{name:'Request context rejected',exact:true}).waitFor();assert.equal(posts.length,1);assert.equal(logoutHits,0);assert.equal(await reply.getByRole('textbox').inputValue(),'Synthetic reply');assert(await reply.getByRole('textbox').getAttribute('readonly')!==null);
 await reply.getByRole('button',{name:'Retry identical reply',exact:true}).click();await new Promise(r=>setTimeout(r,100));assert.equal(posts.length,2);assert.deepEqual(posts[0],posts[1]);await reply.close();checks++;
 console.log(JSON.stringify({checks,status:'PASS',scope:'closed synthetic HTTP + production UI, not live OAuth/member access',auto_auth:0,auto_logout:0,automatic_reply_retries:0}));
}finally{await browser?.close();await new Promise(r=>server.close(r));}
