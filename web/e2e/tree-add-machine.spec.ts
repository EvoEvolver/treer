import {test,expect} from '@playwright/test'
import {mockTreeMapApi,mapSnapshot} from './fixtures/treemap'
for(const width of [1280,390])for(const zh of [false,true])for(const inventory of ['online','offline','empty'] as const)test(`Tree Add machine ${width} ${zh?'zh':'en'} ${inventory}`,async({page},info)=>{
 await page.setViewportSize({width,height:900})
 await page.addInitScript(({zh})=>{localStorage.setItem('treer-lang',zh?'zh':'en');localStorage.setItem('treer-theme',zh?'dark':'light')},{zh})
 await mockTreeMapApi(page)
 await page.route('**/snapshot',r=>r.fulfill({json:{...mapSnapshot,agents:[],servers:inventory==='empty'?[]:mapSnapshot.servers.map(m=>({...m,status:inventory}))}}))
 let bootstrap=0;const writes:string[]=[]
 await page.route('**/bootstrap',r=>{bootstrap++;expect(r.request().method()).toBe('POST');return r.fulfill({json:{install_command:'fixture-install-only',connect_command:'fixture-connect-only'}})})
 page.on('request',r=>{if(!['GET','HEAD','OPTIONS'].includes(r.method())&&!new URL(r.url()).pathname.endsWith('/bootstrap'))writes.push(new URL(r.url()).pathname)})
 await page.goto('/')
 const trigger=page.locator('#tree-sidebar').getByRole('button',{name:zh?'添加到树':'Add to tree',exact:true})
 if(width===390){await page.getByRole('button',{name:zh?'浏览树':'Browse tree',exact:true}).click()}
 // Wait for workspace discovery and its keyed Tree load before keyboard focus.
 await expect(page.locator('#tree-sidebar').getByText('Ideas',{exact:true})).toBeVisible()
 await expect(trigger).toBeEnabled()
 await trigger.focus();await page.keyboard.press('Enter')
 const add=page.getByRole('menuitem',{name:zh?'添加机器':'Add machine',exact:true})
 await expect(add).toBeEnabled()
 await expect(page.getByRole('menuitem',{name:zh?'添加文件夹':'Add folder',exact:true})).toBeVisible()
 const agent=page.getByRole('menuitem',{name:zh?/添加 Agent|先设置机器/:/Add agent|Set up a machine/}).first()
 await expect(agent).toBeVisible()
 if(inventory==='offline')await expect(agent).toBeDisabled()
 await add.focus();await page.keyboard.press('Enter')
 const dialog=page.getByRole('dialog');await expect(dialog.getByRole('heading',{name:zh?'添加机器':'Add machine',exact:true})).toBeVisible()
 await expect(dialog).toContainText('fixture-install-only');await expect(dialog).toContainText('fixture-connect-only')
 expect(bootstrap).toBe(1);expect(writes).toEqual([])
 await expect(page.locator('html')).toHaveJSProperty('scrollWidth',width)
 if(inventory==='empty')await page.screenshot({path:info.outputPath(`add-machine-${width}-${zh?'zh-dark':'en-light'}.png`)})
 await page.keyboard.press('Escape');await expect(dialog).not.toBeVisible();await expect(trigger).toBeFocused()
 expect(bootstrap).toBe(1);expect(writes).toEqual([])
})
