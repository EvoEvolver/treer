// Focused UI boundary, synthetic directory/configuration; no real mail/Core/owner session.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile,mkdir} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.EXTERNAL_PLAYWRIGHT_MODULE));
let mode='proxy',activation=false,paused=true,editable=true,failBind=false,posts=[];
const endpoint={binding:{endpoint_id:'ag_endpoint',recipient:'ag_recipient',expected_path:'/team/endpoint',expected_server_id:'srv_machine',generation:1},revision:7,paused:true,reason:'paused',activity:{activated_at:null,inbound_updates:0,outbound_chunks:0,stale_updates:0}};
const members=[{id:'usr_member',name:'<img src=x onerror=alert(1)> Member',role:'member'},{id:'usr_owner',name:'Owner',role:'owner'}];
const server=createServer(async(req,res)=>{
 res.setHeader('cache-control','no-store');res.setHeader('content-type','application/json');
 const path=req.url;
 if(path==='/api/endpoint/auth/config'){res.end(JSON.stringify({settings_url:editable?`http://127.0.0.1:${server.address().port}/`:'https://dedicated.invalid/'}));return;}
 if(path==='/api/settings'){res.end(JSON.stringify({owner_id:'usr_owner',settings:{activation_enabled:activation,email:mode==='unconfigured'?null:{mode:mode==='outbox'?'outbox':'proxy',worker:null,targets:[{endpoint_id:"ag_endpoint",address:"person@example.test",member_id:"usr_member",revoked:false}],targets_truncated:false},telegram_accounts:[],endpoints:[{...endpoint,paused}],workers:{},audit_full:false}}));return;}
 if(path==='/api/configuration'){
  assert.equal(req.headers['x-endpoint-request'],'1');let raw='';for await(const b of req)raw+=b;const body=JSON.parse(raw);posts.push(body);
  if(body.action==='preview_email_setup'){
   if(mode==='denied'){res.writeHead(401).end('{"error":{"code":"owner_auth_required"}}');return;}
   if(mode==='offline'){res.writeHead(503).end('{"error":{"code":"configuration_unavailable"}}');return;}
   if(mode==='oversize'){res.end(' '.repeat(262145));return;}
   const v={status:'email_setup',email_configured:mode!=='unconfigured',mode:mode==='unconfigured'?null:mode==='outbox'?'outbox':'proxy',activation_enabled:activation,provider_readiness:'unverified',members:mode==='empty'?[]:members,members_truncated:false};
   if(mode==='duplicate')v.members=[members[0],members[0]];if(mode==='invalid')v.provider_readiness='ready';res.end(JSON.stringify(v));return;
  }
  if(body.action==='bind_email'){if(failBind){res.writeHead(503).end('{"error":{"code":"configuration_unavailable"}}');return;}res.end('{"status":"saved"}');return;}
  if(body.action==='inspect_email_delivery'){res.end(JSON.stringify({status:'email_delivery_status',endpoint_id:'ag_endpoint',revision:7,paused:true,delivery:{prepared:1,abandoned:0,dispatching:0,uncertain:1,provider_accepted:1,at_capacity:false},items:[{intent_id:'a'.repeat(64),generation:1,state:'uncertain',terminal:false,abandonable:false,reference:null}],truncated:false}));return;}
  res.writeHead(400).end('{}');return;
 }
 const file={'/':'index.html','/assets/app.js':'app.js','/assets/app.css':'app.css'}[path];if(!file){res.writeHead(404).end('{}');return;}
 res.setHeader('content-type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(await readFile(new URL('../dist/'+file,import.meta.url)));
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const browser=await chromium.launch();let checks=0;
try{
 for(const [width,locale,colorScheme] of [[1280,'en','light'],[390,'zh','dark'],[1280,'zh','dark'],[390,'en','light']]){
  mode='proxy';activation=false;paused=true;editable=true;failBind=false;const initialBinds=posts.filter(x=>x.action==='bind_email').length;
  const context=await browser.newContext({viewport:{width,height:960},locale,colorScheme});const page=await context.newPage();const errors=[];page.on('pageerror',()=>errors.push('pageerror'));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);const card=page.locator('.email-setup');const load=card.locator('button').first();await load.waitFor();assert.match(await card.textContent(),/Unverified|未验证/);assert.match(await card.textContent(),/Runtime gate|运行开关/);assert.equal(posts.filter(x=>x.action==='bind_email').length,initialBinds);checks++;
  await load.click();await card.locator('form').waitFor();assert.equal(await card.locator('select').nth(1).locator('option').count(),3);assert.equal(await card.locator('img').count(),0);assert.match(await card.locator('select').nth(1).textContent(),/<img/);checks++;
  await card.locator('select').nth(0).selectOption('ag_endpoint');await card.locator('select').nth(1).selectOption('usr_member');await card.locator('input[type=email]').fill('person@example.test');const submit=card.locator('form button');assert.equal(await submit.isDisabled(),true);await card.locator('input[type=checkbox]').check();await submit.click();await card.getByRole('status').filter({hasText:/no email|尚未发送/}).waitFor();assert.deepEqual(posts.filter(x=>x.action==='bind_email').at(-1),{action:'bind_email',endpoint_id:'ag_endpoint',revision:7,confirm:true,contact:{address:'person@example.test',member_id:'usr_member'}});assert.match(await card.getByRole('status').textContent(),/no email|尚未发送/);checks++;
  // Changing any selected identity cancels confirmation.
  await card.locator('input[type=checkbox]').check();await card.locator('select').nth(1).selectOption('usr_owner');assert.equal(await card.locator('input[type=checkbox]').isChecked(),false);checks++;
  failBind=true;await card.locator('input[type=checkbox]').check();const before=posts.filter(x=>x.action==='bind_email').length;await submit.click();await card.getByRole('alert').waitFor();await page.waitForTimeout(80);assert.equal(posts.filter(x=>x.action==='bind_email').length,before+1);assert.equal(await card.locator('form').count(),0);assert.equal(await card.locator('input[type=email]').count(),0);checks++;failBind=false;
  for(const state of ['unconfigured','outbox','empty','denied','offline','duplicate','invalid','oversize']){
   mode=state;await load.click();if(['denied','offline','duplicate','invalid','oversize'].includes(state)){await card.getByRole('alert').waitFor();assert.equal(await card.locator('select').count(),0);}else{await card.locator('form').waitFor();if(state==='unconfigured'){assert.match(await card.textContent(),/Not configured|未配置/);assert.equal(await card.locator('form button').isDisabled(),true);}if(state==='outbox')assert.match(await card.textContent(),/Private outbox only|仅写入私有 outbox/);if(state==='empty')assert.equal(await card.locator('select').nth(1).locator('option').count(),1);}checks++;
  }
  mode='proxy';paused=false;await page.reload();await card.locator('button').first().click();await card.locator('form').waitFor();await card.locator('select').nth(0).selectOption('ag_endpoint');await card.locator('select').nth(1).selectOption('usr_member');await card.locator('input[type=email]').fill('person@example.test');await card.locator('input[type=checkbox]').check();assert.equal(await card.locator('form button').isDisabled(),true);checks++;
  // Delivery status read-only inspection has no resend action.
  const recovery=page.locator('.recovery').filter({hasText:locale==='zh'?'邮件发送状态':'Email send status'});await recovery.locator('summary').click();await recovery.getByRole('button',{name:locale==='zh'?'检查邮件投递':'Inspect email delivery',exact:true}).click();await recovery.locator('code').first().waitFor();assert.match(await recovery.textContent(),/Accepted by Treer|Treer 平台已接受/);assert.match(await recovery.textContent(),/not mailbox delivery|不等于收件箱送达/);assert.equal(await recovery.getByRole('button',{name:/resend|重发/i}).count(),0);checks++;
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);for(const el of await card.locator('button,select,input[type=email]').all())assert.ok((await el.boundingBox()).height>=44);checks++;
  if(process.env.EMAIL_SETUP_SCREENSHOTS){await mkdir(process.env.EMAIL_SETUP_SCREENSHOTS,{recursive:true});await page.screenshot({path:`${process.env.EMAIL_SETUP_SCREENSHOTS}/email-setup-${width}-${colorScheme}.png`,fullPage:true});}
  editable=false;const count=posts.length;await page.reload();await card.getByRole('heading').waitFor();await page.waitForTimeout(80);assert.equal(await card.locator('button').count(),0);assert.equal(posts.length,count);checks++;assert.deepEqual(errors,[]);await context.close();
 }
 console.log(`PASS ${checks} email setup/status controls, desktop +390, EN/ZH, light/dark. Synthetic HTTP only; no actual mail/Core/session proof.`);
}finally{await browser.close();await new Promise(r=>server.close(r));}
