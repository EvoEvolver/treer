import assert from 'node:assert/strict';
import {expect} from '@playwright/test';
import {localFetch} from './hermetic.mjs';

// Presentation-only fixture. The separate Rust contract test exercises the
// streaming provider; this checks bounded replacement, keyboard and languages.
export async function testHistoryPages(browser, base, screenshots, pageName) {
  const latest=await (await localFetch(base+'/api/state')).json();
  latest.history={limit:50,cursor:'opaque:64',totalTurns:150};
  latest.thread.hasOlderItems=true;
  const turn=(id,text)=>({id,status:'completed',items:[{id:id+'-message',kind:'agentMessage',text}]});
  latest.thread.turns=[turn('latest-page','Latest history page')];
  for(const lang of ['en','zh']) {
    const context=await browser.newContext({viewport:{width:390,height:844},extraHTTPHeaders:{'x-treer-interface-source':'proxy-ui','x-treer-agent-id':'ag_browser_ui','x-treer-interface-instance':'browser-ui'}});
    try {
      const page=await context.newPage();let socket,olderReads=0,requests=0;
      await page.routeWebSocket('**/ws',ws=>{socket=ws;});
      await page.route('**/api/state',route=>route.fulfill({json:latest}));
      await page.route('**/api/history/older',async route=>{
        const body=route.request().postDataJSON();
        requests++;
        if(requests===2){await route.fulfill({status:429,json:{error:{message:'history_page_busy'}}});return;}
        assert.equal(body.cursor,olderReads?'opaque:32':'opaque:64');olderReads++;
        await route.fulfill({json:{...latest,history:{limit:50,page:true,cursor:olderReads===1?'opaque:32':null,totalTurns:150},thread:{...latest.thread,hasOlderItems:olderReads===1,turns:[turn('old-'+olderReads,'Earlier page '+olderReads)]}}});
      });
      await page.goto(base+'/'+pageName+'?lang='+lang);
      const olderName=lang==='en'?'Load earlier messages':'加载更早的消息';
      const latestName=lang==='en'?'Back to latest messages':'返回最新消息';
      const older=page.getByRole('button',{name:olderName,exact:true});
      await older.focus();await page.keyboard.press('Enter');
      await expect(page.locator('.chat')).toContainText('Earlier page 1');
      await expect(page.locator('.chat')).not.toContainText('Latest history page');
      socket.send(JSON.stringify({...latest,type:'state'}));
      await expect(page.locator('.chat')).toContainText('Earlier page 1');
      await older.click();
      await expect(page.getByRole('alert')).toContainText('history_page_busy');
      await page.waitForTimeout(2100);
      assert.equal(requests,2,'a busy history response must not trigger automatic retries');
      await older.click();
      await expect(page.locator('.chat')).toContainText('Earlier page 2');
      await expect(page.locator('.chat')).not.toContainText('Earlier page 1');
      const back=page.getByRole('button',{name:latestName,exact:true});
      const bounds=await back.boundingBox();assert.ok(bounds.width>=44&&bounds.height>=44);
      assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
      await back.focus();await page.keyboard.press('Enter');
      await expect(page.locator('.chat')).toContainText('Latest history page');
      await expect(page.locator('#thread')).toBeFocused();
      await page.screenshot({path:screenshots+'/history-page-'+lang+'.png'});
      assert.equal(olderReads,2);
      assert.equal(requests,3);
    } finally {await context.close();}
  }
}
