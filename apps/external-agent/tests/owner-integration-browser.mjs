// Focused synthetic HTTP UI controls; no actual owner authorization/public connectivity proof.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {readFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
const {chromium}=await import(pathToFileURL(process.env.EXTERNAL_PLAYWRIGHT_MODULE));
const prefix='/api/workspaces/ws_fixture/apps/app_fixture/ui/';
const identity={workspace:'ws_fixture',app:'app_fixture',agent:'app_fixture'};
const skill='# External Agent\nAPI base URL: https://data.example/\nUse Authorization: Bearer from a private 0600 token file (TOKEN_FILE_PATH). Never put a token in a URL or prompt.\nFirst GET /health, then GET /v1/agents. Permissions come from current App Policy. Do not retry unknown mutations.\n';
let scenario='loopback',mutations=0,downloads=0,holdResolve;
const server=createServer(async(req,res)=>{
 res.setHeader('cache-control','no-store');
 if(!req.url.startsWith(prefix)){res.writeHead(404).end();return;}
 const path=req.url.slice(prefix.length);
 if(req.method!=='GET'){mutations++;res.writeHead(405).end();return;}
 if(path==='owner/state'){res.setHeader('content-type','application/json');res.end(JSON.stringify({identity,generation:'g',enabled:false,activation_available:false,tokens:[]}));return;}
 if(path==='owner/integration'){
  const kind=scenario;
  if(kind==='hold')await new Promise(resolve=>{holdResolve=resolve;});
  res.setHeader('content-type','application/json');
  if(kind==='denied'){res.writeHead(403).end('{}');return;}
  if(kind==='offline'){res.writeHead(503).end('{}');return;}
  if(kind==='oversize'){res.end(' '.repeat(131073));return;}
  const local=kind==='loopback';
  res.end(JSON.stringify({schema_version:1,identity:kind==='foreign'?{...identity,app:'foreign'}:identity,data_base_url:kind==='unsafe'?'https://owner.example/ui/?token=bad':local?'http://127.0.0.1:62921/':'https://data.example/',reachability:local?'loopback_only':'configured_https_unverified',enabled:!local,activation_available:!local,prompt_available:false,blockers:local?['loopback_only','disabled','policy_acceptance_missing']:['data_mapping_not_verified'],skill_markdown:local?skill.replaceAll('https://data.example/','http://127.0.0.1:62921/'):skill,manual_path:'SKILL.md'}));return;
 }
 if(path==='SKILL.md'){downloads++;res.setHeader('content-type','text/markdown');res.end(skill);return;}
 const files={'':'settings.html','settings.js':'settings.js','settings.css':'settings.css'};
 if(!files[path]){res.writeHead(404).end();return;}
 res.setHeader('content-type',path.endsWith('.js')?'application/javascript':path.endsWith('.css')?'text/css':'text/html');res.end(await readFile(new URL('../src/runtime/'+files[path],import.meta.url)));
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser=await chromium.launch({headless:true});let cases=0;
try{
 for(const [width,locale,colorScheme] of [[1100,'en','light'],[390,'zh','dark']]){
  const context=await browser.newContext({viewport:{width,height:844},locale,colorScheme});
  await context.addInitScript(()=>{globalThis.copied=[];Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>globalThis.copied.push(text)}});});
  const page=await context.newPage();scenario='loopback';await page.goto(`http://127.0.0.1:${server.address().port}${prefix}`);
  await page.locator('#integration-content').waitFor();
  assert.equal(await page.locator('#integration-base').textContent(),'http://127.0.0.1:62921/');
  assert.ok((await page.locator('#integration-blockers li').count())>=3);
  await page.evaluate(()=>{document.getElementById('secret').value='SYNTHETIC_TOKEN_NEVER_IN_SKILL';});
  await page.locator('#integration-copy').focus();await page.keyboard.press('Enter');
  await page.waitForFunction(()=>copied.length===1);
  let copied=await page.evaluate(()=>copied.at(-1));
  assert.ok(copied.includes('127.0.0.1:62921'));assert.ok(!copied.includes('SYNTHETIC_TOKEN'));assert.ok(!copied.includes('owner.example'));
  assert.ok(/Connection snapshot|连接状态快照/.test(copied));cases++;
  scenario='unverified';await page.locator('#integration-refresh').click();await page.waitForFunction(()=>document.getElementById('integration-base').textContent==='https://data.example/');
  assert.ok(/Not ready|尚未就绪/.test(await page.locator('#integration-status').textContent()));assert.ok(!(await page.locator('#integration-blockers').textContent()).includes('additional readiness'));
  await page.locator('#integration-copy').click();await page.waitForFunction(()=>copied.length===2);
  copied=await page.evaluate(()=>copied.at(-1));assert.ok(copied.includes('Authorization: Bearer'));assert.ok(copied.indexOf('/health')<copied.indexOf('/v1/agents'));assert.ok(copied.includes('Policy'));assert.ok(copied.includes('TOKEN_FILE_PATH'));assert.ok(!copied.includes('Provide the token separately'));assert.ok(!copied.includes('通过 Agent 的秘密环境变量输入'));cases++;
  const downloadEvent=page.waitForEvent('download');await page.locator('#integration-download').click();const download=await downloadEvent;assert.equal(download.suggestedFilename(),'SKILL.md');const data=await readFile(await download.path(),'utf8');assert.equal(data,skill);cases++;
  await page.evaluate(()=>{navigator.clipboard.writeText=async()=>{throw Error('denied');};});await page.locator('#integration-copy').click();
  assert.equal(await page.locator('#integration-text').evaluate(node=>node.selectionEnd-node.selectionStart),await page.locator('#integration-text').evaluate(node=>node.value.length));
  assert.ok(/Ctrl\/Cmd\+C/.test(await page.locator('#integration-feedback').textContent()));cases++;
  await page.selectOption('#language',locale==='zh'?'en':'zh');assert.equal(await page.locator('#integration-feedback').textContent(),'');
  assert.ok((await page.locator('#integration-copy').textContent()).includes(locale==='zh'?'Copy':'复制'));
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
  for(const selector of ['#integration-refresh','#integration-copy','#integration-download'])assert.ok((await page.locator(selector).boundingBox()).height>=44);
  if(width===390&&process.env.INTEGRATION_SCREENSHOT)await page.screenshot({path:process.env.INTEGRATION_SCREENSHOT,fullPage:true});
  for(const kind of ['denied','offline','foreign','oversize','unsafe']){
   scenario=kind;await page.locator('#integration-refresh').click();await page.waitForFunction(()=>!document.getElementById('integration-refresh').disabled);
   assert.equal(await page.locator('#integration-content').isVisible(),false);assert.equal(await page.locator('#integration-text').inputValue(),'');assert.equal(await page.locator('#integration-copy').isEnabled(),false);assert.ok((await page.locator('#integration-status').textContent()).length>0);cases++;
  }
  scenario='hold';await page.locator('#integration-refresh').click();await page.waitForFunction(()=>document.getElementById('integration-refresh').disabled);
  // A queued old request must not restore connection data after leaving the page.
  await page.evaluate(()=>dispatchEvent(new Event('pagehide')));holdResolve();
  await page.waitForTimeout(50);assert.equal(await page.locator('#integration-content').isVisible(),false);assert.equal(await page.locator('#integration-text').inputValue(),'');cases++;
  await context.close();
 }
 assert.equal(mutations,0);assert.equal(downloads,2);console.log(`PASS ${cases} focused connection UI controls across desktop light/en + mobile dark/zh; prefix/download/copy/fallback/stale/invalid/denied, zero mutations. Synthetic HTTP only.`);
}finally{if(holdResolve)holdResolve();await browser.close();await new Promise(resolve=>server.close(resolve));}
