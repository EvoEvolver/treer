// Focused browser regression for the default Console/API origin pair.
// This uses a temporary local server and a synthetic cookie, never live Treer.
import assert from 'node:assert/strict';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {test} from 'node:test';
import {chromium,firefox,webkit} from '../../web/node_modules/playwright/index.mjs';
const engine=process.env.SINGLE_CONTAINER_BROWSER || 'chromium';
const useProxy=process.env.SINGLE_CONTAINER_DNS_PROXY === '1';
assert(['chromium','firefox','webkit'].includes(engine));

test(`${engine}: documented Strict-cookie behavior (${useProxy?'mapped .test':'loopback names'})`, async()=>{
  const temporary=fs.mkdtempSync(path.join(os.tmpdir(),'treer-single-cookie-'));
  const server=http.createServer((request,response)=>{
    const route=new URL(request.url,'http://stub.invalid').pathname;
    if(request.headers.origin){
      // Deliberately permissive on this loopback test server, not product code.
      response.setHeader('Access-Control-Allow-Origin',request.headers.origin);
      response.setHeader('Access-Control-Allow-Credentials','true');
    }
    if(route==='/login') response.setHeader('Set-Cookie','synthetic=local-only; Path=/; HttpOnly; SameSite=Strict');
    if(route==='/who'){
      response.setHeader('Content-Type','application/json');
      response.end(JSON.stringify({has_cookie:!!request.headers.cookie}));
    }else{
      response.setHeader('Content-Type','text/html');
      response.end('<!doctype html><title>Local cookie regression</title>');
    }
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  let browser;
  try{
    const port=server.address().port;
    browser=await ({chromium,firefox,webkit}[engine]).launch({headless:true,
      ...(useProxy?{proxy:{server:`http://127.0.0.1:${port}`,bypass:''}}:{}),
      env:{PATH:process.env.PATH,HOME:temporary,TMPDIR:temporary,LANG:'en_US.UTF-8'}});
    const pairs=useProxy?[
      ['console.treer.test','proxy.treer.test',true],
    ]:[
      ['localhost','127.0.0.1',false],
      // WebKit's localhost limitation is documented; the .test mapping is its
      // separately tested alternative, not a reason to weaken cookie policy.
      ['console.treer.localhost','proxy.treer.localhost',engine!=='webkit'],
    ];
    for(const [app,proxy,expected] of pairs){
      const context=await browser.newContext();
      try{
        const page=await context.newPage();
        await page.goto(`http://${proxy}:${port}/login`);
        assert.equal((await context.cookies()).length,1);
        await page.goto(`http://${app}:${port}/`);
        const result=await page.evaluate(async url=>(await fetch(url,{credentials:'include'})).json(),`http://${proxy}:${port}/who`);
        assert.equal(result.has_cookie,expected,`${app} → ${proxy}`);
      }finally{await context.close();}
    }
  }finally{
    await browser?.close();
    await new Promise(resolve=>server.close(resolve));
    fs.rmSync(temporary,{recursive:true,force:true});
  }
});
