import { test, expect, type Page, type WebSocketRoute } from '@playwright/test'
import { mockTreeMapApi, mapSnapshot } from './fixtures/treemap'
import { browseTreeOnMobile } from './fixtures/navigation'
type Pin = {kind:'agent'|'app'|'folder';id:string;pinned_at:string;pinned_by:string|null}
const pin = (kind:Pin['kind'],id:string):Pin => ({kind,id,pinned_at:'2026-10-17T12:00:00Z',pinned_by:'removed-user'})
async function backend(page:Page, initial:Pin[] = [], options:{canPin?:boolean;supported?:boolean} = {}) {
  await mockTreeMapApi(page)
  let pins = initial, canPin = options.canPin ?? true, failure = 0, writeFailure = 0, reads = 0
  let snapshot = mapSnapshot
  const writes:string[] = []
  let socket:WebSocketRoute | undefined
  await page.routeWebSocket(/\/events$/, (connection) => {socket=connection})
  await page.route('**/auth/config', (route) => route.fulfill({json:{capabilities:options.supported === false ? []:['workspace.pins.v1']}}))
  await page.route('**/snapshot', (route) => route.fulfill({json:snapshot}))
  await page.route('**/apps', (route) => route.fulfill({json:{apps:[{app_id:'dashboard-app',runtime_agent_id:'dashboard-runtime',name:'Dashboard',server_id:'local',workspace_id:'map-workspace',status:'running'}]}}))
  await page.route('**/pins**', (route) => {
    const method = route.request().method(), parts = new URL(route.request().url()).pathname.split('/')
    if (method === 'GET') {
      reads++
      return route.fulfill({status:failure || 200,json:failure ? {}:{pins,limit:50,can_pin:canPin,revision:reads}})
    }
    writes.push(`${method} ${parts.at(-2)}/${parts.at(-1)}`)
    if (failure || writeFailure) return route.fulfill({status:failure || writeFailure,json:{error:{code:'pins_limit_reached'}}})
    const kind = parts.at(-2) as Pin['kind'], id = parts.at(-1)!
    pins = pins.filter((p) => !(p.kind === kind && p.id === id))
    if (method === 'PUT') pins.unshift(pin(kind,id))
    return method === 'DELETE' ? route.fulfill({status:204,body:''}):route.fulfill({json:pin(kind,id)})
  })
  return {writes, setWriteFailure:(status:number) => {writeFailure=status}, get reads(){return reads},setFailure:(status:number) => {failure=status},setCanPin:(value:boolean) => {canPin=value},
    change:(next:Pin[]) => {pins=next;socket!.send(JSON.stringify({event:'workspace.pins.changed',data:{action:'pinned',kind:'agent',id:'leader',revision:99}}))},
    snapshot:(next:typeof mapSnapshot) => {snapshot=next;socket!.send(JSON.stringify({event:'workspace.snapshot',data:next}))}}
}
const overview = (page:Page) => page.getByRole('region',{name:'Workspace overview tree',exact:true})
const pinned = (page:Page) => page.getByRole('region',{name:'Pinned',exact:true})
for (const mobile of [false,true]) for (const theme of ['light','dark']) {
  test(`shared pins menus, folders, App identity and reload ${theme} ${mobile ? '390':'desktop'}`,async ({page},info) => {
    await page.setViewportSize({width:mobile ? 390:1360,height:900})
    await page.addInitScript((theme) => localStorage.setItem('treer-theme',theme),theme)
    const api = await backend(page)
    await page.goto('/')
    await expect(overview(page).getByRole('button',{name:'Actions for /coordinator',exact:true})).toBeVisible()
    await expect(pinned(page)).toHaveCount(0)
    await overview(page).getByRole('button',{name:'Actions for /coordinator',exact:true}).click()
    await page.getByRole('menuitem',{name:'Pin to Overview',exact:true}).click()
    await expect(pinned(page).getByRole('link',{name:'Open /coordinator',exact:true})).toBeVisible()
    await browseTreeOnMobile(page)
    await page.getByRole('treeitem',{name:'Research',exact:true}).click({button:'right'})
    await page.getByRole('menuitem',{name:'Pin to Overview',exact:true}).click()
    await page.getByRole('treeitem',{name:'Dashboard',exact:true}).getByRole('button',{name:'Actions for Dashboard',exact:true}).click()
    await page.getByRole('menuitem',{name:'Pin to Overview',exact:true}).click()
    expect(api.writes).toEqual(['PUT agent/leader','PUT folder/research','PUT app/dashboard-app'])
    if (mobile) await page.getByRole('button',{name:'Workspace overview',exact:true}).click()
    await expect(pinned(page).getByRole('link',{name:'Open /Dashboard',exact:true})).toHaveAttribute('href',/dashboard-runtime$/)
    await pinned(page).getByRole('button',{name:'Folder /Research',exact:true}).click()
    await expect(pinned(page).getByRole('link',{name:'Open /Research/reviewer',exact:true})).toBeVisible()
    await page.screenshot({path:info.outputPath(`pins-${theme}-${mobile ? '390':'desktop'}.png`)})
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.reload()
    await expect(pinned(page).getByRole('link',{name:'Open /coordinator',exact:true})).toBeVisible()
    await pinned(page).getByRole('button',{name:'Actions for /coordinator',exact:true}).click()
    await page.getByRole('menuitem',{name:'Unpin',exact:true}).click()
    await expect(pinned(page).getByRole('link',{name:'Open /coordinator',exact:true})).toHaveCount(0)
    expect(api.writes.at(-1)).toBe('DELETE agent/leader')
  })
}
test('shared event re-reads authoritative pins, rename follows ID, deletion hides dangling pins',async ({page}) => {
  const api = await backend(page)
  await page.goto('/')
  await expect(overview(page).getByRole('button',{name:'Actions for /coordinator',exact:true})).toBeVisible()
  api.change([pin('agent','leader')])
  await expect(pinned(page).getByRole('link',{name:'Open /coordinator',exact:true})).toBeVisible()
  api.snapshot({...mapSnapshot,revision:2,agents:mapSnapshot.agents.map((a) => a.agent_id === 'leader' ? {...a,name:'Renamed'}:a)})
  await expect(pinned(page).getByRole('link',{name:'Open /Renamed',exact:true})).toBeVisible()
  api.snapshot({...mapSnapshot,revision:3,agents:mapSnapshot.agents.filter((a) => a.agent_id !== 'leader')})
  await expect(pinned(page)).toHaveCount(0)
  expect(api.writes).toEqual([])
})
test('read-only members see shared pins without pin menus',async ({page}) => {
  await backend(page,[pin('agent','leader')],{canPin:false})
  await page.goto('/')
  await expect(pinned(page).getByRole('link',{name:'Open /coordinator',exact:true})).toBeVisible()
  await expect(overview(page).getByRole('button',{name:/Actions for/})).toHaveCount(0)
  await page.getByRole('treeitem',{name:'coordinator',exact:true}).click({button:'right'})
  await expect(page.getByRole('menuitem',{name:/Pin to Overview|Unpin/})).toHaveCount(0)
})
test('old Proxy hides all pins affordances without local storage fallback',async ({page}) => {
  const api = await backend(page,[pin('agent','leader')],{supported:false})
  await page.goto('/')
  await expect(overview(page).getByRole('link',{name:'Open /coordinator',exact:true})).toBeVisible()
  await expect(pinned(page)).toHaveCount(0)
  await expect(overview(page).getByRole('button',{name:/Actions for/})).toHaveCount(0)
  expect(api.reads).toBe(0)
})
for (const status of [403,409,503]) test(`pin write ${status} is explicit, no automatic retry`,async ({page}) => {
  const api = await backend(page,[pin('agent','leader')])
  await page.goto('/')
  await expect(pinned(page).getByRole('button',{name:'Actions for /coordinator',exact:true})).toBeVisible()
  api.setWriteFailure(status)
  await pinned(page).getByRole('button',{name:'Actions for /coordinator',exact:true}).click()
  await page.getByRole('menuitem',{name:'Unpin',exact:true}).click()
  await expect(page.getByRole('alert').first()).toContainText(status === 409 ? '50-pin limit':'Unable to change')
  expect(api.writes).toEqual(['DELETE agent/leader'])
  if (status === 403) await expect(pinned(page)).toHaveCount(0)
  else await expect(pinned(page).getByRole('link',{name:'Open /coordinator',exact:true})).toBeVisible()
})
test('GET denial clears pins and menus; foreground reconcile restores authorized state',async ({page}) => {
  const api = await backend(page,[pin('agent','leader')])
  await page.goto('/')
  await expect(pinned(page)).toBeVisible()
  api.setFailure(403)
  await page.evaluate(() => dispatchEvent(new Event('focus')))
  await expect(pinned(page)).toHaveCount(0)
  await expect(page.getByRole('alert').first()).toContainText('Unable to load')
  await expect(overview(page).getByRole('button',{name:/Actions for/})).toHaveCount(0)
  api.setFailure(0);api.setCanPin(false)
  await page.evaluate(() => dispatchEvent(new Event('focus')))
  await expect(pinned(page)).toBeVisible()
  await expect(overview(page).getByRole('button',{name:/Actions for/})).toHaveCount(0)
})

test('workspace switches discard a late old-workspace pins read',async ({page}) => {
  await backend(page,[pin('agent','leader')])
  await page.route('**/workspaces?*', (route) => route.fulfill({json:{workspaces:[mapSnapshot.workspace,{...mapSnapshot.workspace,workspace_id:'second',name:'Second'}]}}))
  await page.route('**/snapshot', (route) => route.fulfill({json:{...mapSnapshot,workspace:new URL(route.request().url()).pathname.includes('/second/') ? {...mapSnapshot.workspace,workspace_id:'second',name:'Second'}:mapSnapshot.workspace}}))
  await page.goto('/')
  await expect(pinned(page)).toBeVisible()
  let release!:() => void, started=false, finished=false
  const pending=new Promise<void>((resolve) => {release=resolve})
  await page.route('**/pins',async (route) => {
    if (new URL(route.request().url()).pathname.includes('/map-workspace/')) {
      started=true;await pending
      await route.fulfill({json:{pins:[pin('agent','leader')],limit:50,can_pin:true,revision:500}})
      finished=true
    } else await route.fulfill({json:{pins:[],limit:50,can_pin:false,revision:1}})
  })
  await page.evaluate(() => dispatchEvent(new Event('focus')))
  await expect.poll(() => started).toBe(true)
  await page.getByRole('combobox',{name:'Workspace',exact:true}).click()
  await page.getByRole('option',{name:'Second',exact:true}).click()
  await expect(page).toHaveURL(/\/workspaces\/second$/)
  release()
  await expect.poll(() => finished).toBe(true)
  await expect(pinned(page)).toHaveCount(0)
  await expect(overview(page).getByRole('button',{name:/Actions for/})).toHaveCount(0)
})

for (const malformed of ['permission','duplicate','limit']) test(`malformed pins ${malformed} fail closed`,async ({page}) => {
  await backend(page)
  await page.route('**/pins',(route) => route.fulfill({json:{pins:malformed === 'duplicate' ? [pin('agent','leader'),pin('agent','leader')]:[pin('agent','leader')],limit:malformed === 'limit' ? 500:50,can_pin:malformed === 'permission' ? 'true':true,revision:1}}))
  await page.goto('/')
  await expect(page.getByRole('alert').first()).toContainText('Unable to load')
  await expect(pinned(page)).toHaveCount(0)
  await expect(overview(page).getByRole('button',{name:/Actions for/})).toHaveCount(0)
})

test('pin order follows server order and stays visible under attention filtering',async ({page}) => {
  await backend(page,[pin('agent','leader'),pin('folder','product')])
  await page.goto('/')
  await expect(pinned(page).locator('li').first()).toContainText('coordinator')
  await page.getByRole('button',{name:'Filter need attention',exact:true}).click()
  await expect(pinned(page).getByRole('link',{name:'Open /coordinator',exact:true})).toBeVisible()
  await expect(pinned(page).getByRole('button',{name:'Folder /Product',exact:true})).toBeVisible()
})
for (const mobile of [false,true]) for (const theme of ['light','dark']) test(`Chinese shared pins ${theme} ${mobile ? '390':'desktop'}`,async ({page},info) => {
  await page.setViewportSize({width:mobile ? 390:1360,height:900})
  await page.addInitScript((theme) => {localStorage.setItem('treer-theme',theme);localStorage.setItem('treer-lang','zh')},theme)
  await backend(page,[pin('app','dashboard-app'),pin('folder','research'),pin('agent','leader')])
  await page.goto('/')
  const section=page.getByRole('region',{name:'已置顶',exact:true})
  await expect(section).toBeVisible()
  await expect(section.getByRole('button',{name:'/coordinator 的操作',exact:true})).toHaveAttribute('title',/未知成员/)
  await page.screenshot({path:info.outputPath(`pins-zh-${theme}-${mobile ? '390':'desktop'}.png`)})
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})


test('late successful GET cannot restore pins after a denied write', async ({page}) => {
  const api = await backend(page, [pin('agent','leader')])
  await page.goto('/')
  await expect(pinned(page)).toBeVisible()
  let release!: () => void, started = false, finished = false
  const pending = new Promise<void>((resolve) => { release = resolve })
  await page.route('**/pins', async (route) => {
    started = true
    await pending
    await route.fulfill({json:{pins:[pin('agent','leader')],limit:50,can_pin:true,revision:900}})
    finished = true
  })
  await page.evaluate(() => dispatchEvent(new Event('focus')))
  await expect.poll(() => started).toBe(true)
  api.setWriteFailure(403)
  await pinned(page).getByRole('button',{name:'Actions for /coordinator',exact:true}).click()
  await page.getByRole('menuitem',{name:'Unpin',exact:true}).click()
  await expect(pinned(page)).toHaveCount(0)
  release()
  await expect.poll(() => finished).toBe(true)
  await expect(page.getByRole('alert').first()).toContainText('Unable to change')
  await expect(overview(page).getByRole('button',{name:/Actions for/})).toHaveCount(0)
})

test('Overview pin menu is keyboard accessible and restores focus', async ({page}) => {
  await backend(page)
  await page.goto('/')
  const trigger = overview(page).getByRole('button',{name:'Actions for /coordinator',exact:true})
  await trigger.focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('menuitem',{name:'Pin to Overview',exact:true})).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(pinned(page)).toBeVisible()
  await expect(trigger).toBeFocused()
})

test('pins follow stable Agent and folder IDs across moves', async ({page}) => {
  const api = await backend(page, [pin('agent','leader'),pin('folder','research')])
  let moved = false
  await page.route('**/workload-tree', (route) => route.fulfill({json:{
    folders:[{folder_id:'product',name:'Product',parent_id:null},{folder_id:'research',name:'Research',parent_id:moved ? 'product':null}],
    placements:moved ? [{agent_id:'leader',folder_id:'research',position:0}]:[],
  }}))
  await page.goto('/')
  await expect(pinned(page).getByRole('link',{name:'Open /coordinator',exact:true})).toBeVisible()
  await expect(pinned(page).getByRole('button',{name:'Folder /Research',exact:true})).toBeVisible()
  moved = true
  await page.evaluate(() => dispatchEvent(new Event('focus')))
  await expect(pinned(page).getByRole('link',{name:'Open /Product/Research/coordinator',exact:true})).toBeVisible()
  await expect(pinned(page).getByRole('button',{name:'Folder /Product/Research',exact:true})).toBeVisible()
  expect(api.writes).toEqual([])
})


for (const status of [404,503]) test(`unavailable capability discovery ${status} hides pins without unrelated errors`, async ({page}) => {
  const api = await backend(page)
  await page.route('**/auth/config', (route) => route.fulfill({status,json:{error:'unavailable'}}))
  await page.goto('/')
  await expect(overview(page).getByRole('link',{name:'Open /coordinator',exact:true})).toBeVisible()
  await expect(pinned(page)).toHaveCount(0)
  await expect(page.getByRole('alert')).toHaveCount(0)
  expect(api.reads).toBe(0)
})

for (const mobile of [false,true]) test(`pins menu and error state gallery ${mobile ? '390':'desktop'}`, async ({page},info) => {
  await page.setViewportSize({width:mobile ? 390:1360,height:900})
  const api = await backend(page)
  await page.goto('/')
  const shot = async (state:string) => {
    await page.mouse.move(1,1)
    await page.screenshot({path:info.outputPath(`pins-state-${state}-${mobile ? '390':'desktop'}.png`)})
  }
  const actions = () => overview(page).getByRole('button',{name:'Actions for /coordinator',exact:true})
  await expect(actions()).toBeVisible()
  await shot('empty')
  await actions().click()
  await expect(page.getByRole('menuitem',{name:'Pin to Overview',exact:true})).toBeVisible()
  await shot('pin-menu')
  await page.getByRole('menuitem',{name:'Pin to Overview',exact:true}).click()
  await pinned(page).getByRole('button',{name:'Actions for /coordinator',exact:true}).click()
  await expect(page.getByRole('menuitem',{name:'Unpin',exact:true})).toBeVisible()
  await shot('unpin-menu')
  await page.keyboard.press('Escape')
  for (const status of [409,503,403]) {
    api.setWriteFailure(status)
    await actions().click()
    await page.getByRole('menuitem',{name:'Unpin',exact:true}).click()
    await expect(page.getByRole('alert').first()).toContainText(status === 409 ? '50-pin limit':'Unable to change')
    await page.locator('#workspace-panel-overview').getByRole('alert').scrollIntoViewIfNeeded()
    await shot(`error-${status}`)
  }
  api.setWriteFailure(0);api.setCanPin(false)
  await page.evaluate(() => dispatchEvent(new Event('focus')))
  await expect(pinned(page)).toBeVisible()
  await expect(actions()).toHaveCount(0)
  await shot('read-only')
  api.snapshot({...mapSnapshot,revision:10,agents:mapSnapshot.agents.filter((a) => a.agent_id !== 'leader')})
  await expect(pinned(page)).toHaveCount(0)
  await shot('deleted')
})


test('Core omits a deleted pin author rather than serializing null', async ({page}) => {
  await backend(page)
  await page.route('**/pins', (route) => route.fulfill({json:{pins:[{kind:'agent',id:'leader',pinned_at:'2026-10-17T12:00:00Z'}],limit:50,can_pin:true,revision:1}}))
  await page.goto('/')
  await expect(pinned(page).getByRole('button',{name:'Actions for /coordinator',exact:true})).toHaveAttribute('title',/Unknown member/)
  await expect(page.getByRole('alert')).toHaveCount(0)
})
