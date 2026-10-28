import { openTabMenu, tabAction } from "./fixtures/tab-actions"
import { test, expect, type Page } from '@playwright/test'
import { fixture, gotoWorkspace, open, switchTab } from './fixtures/workspace-tabs'
const storageKey = 'treer:tabs:v1:map-user:map-workspace'
for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`tabs retain iframe drafts, scroll and terminal connection ${theme} ${mobile ? '390' : 'desktop'}`, async ({ page }, info) => {
    await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
    await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
    const backend = await fixture(page)
    await gotoWorkspace(page)
    await open(page, 'Alpha', mobile)
    const alpha = page.frameLocator('#workspace-panel-tab-1 iframe')
    await alpha.getByRole('textbox', { name: 'Draft' }).fill('Private draft remains in memory only')
    const boot = await page.locator('#workspace-panel-tab-1 iframe').evaluate((iframe: HTMLIFrameElement) => { iframe.contentWindow!.scrollTo(0, 400); return (iframe.contentWindow as any).bootId })
    await open(page, 'Terminal', mobile)
    await expect(page.locator('.xterm')).toBeVisible()
    await expect.poll(() => backend.connections.length).toBe(1)
    await open(page, 'Beta', mobile)
    await switchTab(page, 'Alpha', mobile)
    await expect(alpha.getByRole('textbox', { name: 'Draft' })).toHaveValue('Private draft remains in memory only')
    expect(await page.locator('#workspace-panel-tab-1 iframe').evaluate((iframe: HTMLIFrameElement) => ({ boot: (iframe.contentWindow as any).bootId, scroll: iframe.contentWindow!.scrollY }))).toEqual({ boot, scroll: 400 })
    expect(backend.connections).toEqual(['tab-0']); expect(backend.closed).toEqual([])
    backend.output('tab-0', 'background output while hidden\r\n')
    expect(await page.evaluate((key) => localStorage.getItem(key), storageKey)).toBe('["tab-1","tab-0","tab-2"]')
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.locator('#workspace-panel-tab-1 iframe').evaluate((iframe: HTMLIFrameElement) => iframe.contentWindow!.scrollTo(0, 0))
    await page.screenshot({ path: info.outputPath(`tabs-${theme}-${mobile ? '390' : 'desktop'}.png`) })
    if (mobile) {
      await page.getByRole('button', { name: /^Show all tabs/ }).click()
      await page.screenshot({ path: info.outputPath(`tabs-${theme}-390-switcher.png`) })
      await expect(page.getByRole('dialog', { name: /^Open tabs/ })).toBeVisible()
      await page.getByRole('button', { name: 'Done', exact: true }).click()
    }
    await switchTab(page, 'Terminal', mobile)
    await expect(page.locator('.xterm-screen')).toContainText('retained terminal output')
    await expect(page.locator('.xterm-screen')).toContainText('background output while hidden')
    await page.locator('.xterm-helper-textarea').press('a')
    await expect.poll(() => backend.inputs).toContain('a')
    await switchTab(page, 'Overview', mobile)
    expect(backend.closed).toEqual([])
  })
}
test('tabs have roving keyboard navigation, shortcuts and close fallback', async ({ page }) => {
  const backend = await fixture(page)
  await gotoWorkspace(page)
  await open(page, 'Alpha'); await open(page, 'Beta'); await open(page, 'Terminal')
  await page.locator('.xterm-helper-textarea').press('Control+w')
  await expect.poll(() => backend.inputs).toContain('\x17')
  await expect(page.getByRole('tab', { name: 'Tab /Terminal', exact: true })).toHaveCount(1)
  const first = page.getByRole('tab', { name: 'Tab Overview', exact: true })
  await first.focus(); await page.keyboard.press('ArrowRight')
  await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toBeFocused()
  await page.keyboard.press('End')
  await expect(page.getByRole('tab', { name: 'Tab /Terminal', exact: true })).toBeFocused()
  await page.keyboard.press('Home'); await expect(first).toBeFocused()
  await page.keyboard.press('Control+3')
  await expect(page.getByRole('tab', { name: 'Tab /Beta', exact: true })).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('Control+w')
  await expect(page.getByRole('tab', { name: 'Tab /Beta', exact: true })).toHaveCount(0)
  const tty = page.getByRole('tab', { name: 'Tab /Terminal', exact: true })
  await tty.focus(); await page.keyboard.press('Delete')
  await expect(tty).toHaveCount(0)
  await expect.poll(() => backend.closed).toContain('tab-0')
  await page.getByRole('button', { name: 'Close tab /Alpha', exact: true }).click()
  await expect(first).toHaveAttribute('aria-selected', 'true')
  await expect(page.getByRole('button', { name: 'Close tab Overview', exact: true })).toHaveCount(0)
})
test('App tab stays mounted through Agent, overview and settings; log choice is local', async ({ page }) => {
  const backend = await fixture(page)
  await gotoWorkspace(page)
  await open(page, 'Archive')
  const frame = page.frameLocator('#workspace-panel-tab-7 iframe')
  await frame.getByRole('textbox', { name: 'Draft' }).fill('App draft')
  await open(page, 'Terminal'); await switchTab(page, 'Archive')
  await expect(frame.getByRole('textbox', { name: 'Draft' })).toHaveValue('App draft')
  await page.getByRole('button', { name: 'Workspace settings', exact: true }).click()
  await switchTab(page, 'Archive')
  await expect(frame.getByRole('textbox', { name: 'Draft' })).toHaveValue('App draft')
  await tabAction(page, 'Show logs')
  await expect.poll(() => backend.connections).toContain('tab-7')
  await open(page, 'Alpha'); await switchTab(page, 'Archive')
  { await openTabMenu(page); await expect(page.getByRole('menuitem', { name: 'Show App', exact: true })).toBeVisible(); await page.keyboard.press('Escape') }
  await expect(page.locator('#workspace-panel-tab-7 .xterm')).toBeVisible()
})

for (const theme of ['light', 'dark']) for (const surface of ['Terminal', 'Alpha']) {
  test(`mobile top bar returns directly to Tree and retains ${surface} ${theme}`, async ({ page }, info) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
    const backend = await fixture(page)
    await gotoWorkspace(page)
    await open(page, surface, true)
    const frame = page.frameLocator('#workspace-panel-tab-1 iframe')
    if (surface === 'Alpha') await frame.getByRole('textbox', { name: 'Draft' }).fill('Retain this draft')
    const connections = [...backend.connections]
    const browse = page.getByRole('button', { name: 'Browse tree', exact: true })
    await expect(browse).toBeVisible()
    await expect(browse).toHaveAttribute('aria-controls', 'tree-sidebar')
    const hit = (await browse.boundingBox())!
    expect(hit.width).toBeGreaterThanOrEqual(44)
    expect(hit.height).toBeGreaterThanOrEqual(44)
    await page.screenshot({ path: info.outputPath(`tree-return-${surface}-${theme}-390.png`) })
    await browse.focus()
    await page.keyboard.press('Enter')
    await expect(page.locator('#tree-sidebar')).toBeVisible()
    await expect(page.getByRole('heading', { name: 'Workspace overview', exact: true })).not.toBeVisible()
    await expect(page.locator(`[data-agent-id="tab-${surface === 'Alpha' ? 1 : 0}"]`)).toHaveAttribute('aria-selected', 'true')
    const selectedRow = page.locator(`[data-agent-id="tab-${surface === 'Alpha' ? 1 : 0}"] [data-tree-primary]`)
    await expect(selectedRow).toBeFocused()
    await selectedRow.click()
    if (surface === 'Alpha') await expect(frame.getByRole('textbox', { name: 'Draft' })).toHaveValue('Retain this draft')
    else expect(backend.connections).toEqual(connections)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  })
}

const workspaceUrl = '/orgs/map-org/workspaces/map-workspace'
test.describe('workload deep links and new-tab entry points', () => {
  test('a deep link opens that workload, keeps the URL, and the URL follows the selection', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await page.goto(`${workspaceUrl}/workloads/tab-1`)
    await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
    await expect(page.locator('#workspace-panel-tab-1 iframe')).toBeVisible()
    await expect(page).toHaveURL(new RegExp(`${workspaceUrl}/workloads/tab-1$`))
    await page.reload()
    await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
    await switchTab(page, 'Overview')
    await expect(page).toHaveURL(new RegExp(`${workspaceUrl}$`))
    await switchTab(page, 'Alpha')
    await expect(page).toHaveURL(new RegExp(`${workspaceUrl}/workloads/tab-1$`))
  })

  test('an unknown workload id reports a clear error and selects nothing else', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await page.goto(`${workspaceUrl}/workloads/not-a-workload`)
    await expect(page.getByRole('alert').filter({ hasText: 'not found, or you do not have access' })).toBeVisible()
    await expect(page).toHaveURL(new RegExp(`${workspaceUrl}$`))
    await expect(page.getByRole('tab', { name: 'Tab Overview', exact: true })).toHaveAttribute('aria-selected', 'true')
    await page.goto(`${workspaceUrl}/workloads/..%2Fsecret`)
    await expect(page.getByRole('tab', { name: 'Tab Overview', exact: true })).toHaveAttribute('aria-selected', 'true')
    await expect(page.locator('iframe')).toHaveCount(0)
  })

  test('a deep link into a missing workspace or organization reports the error and opens nothing else', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    for (const url of ['/orgs/map-org/workspaces/missing/workloads/tab-1', '/orgs/missing/workspaces/map-workspace/workloads/tab-1']) {
      await page.goto(url)
      await expect(page.getByRole('alert').filter({ hasText: 'not found, or you do not have access' })).toBeVisible()
      // The accessible workspace was not substituted: no overview of it, no workload, URL untouched.
      await expect(page.getByRole('link', { name: 'Open /Alpha', exact: true })).toHaveCount(0)
      await expect(page.getByRole('tree').getByRole('treeitem')).toHaveCount(0)
      await expect(page.locator('iframe,.xterm')).toHaveCount(0)
      await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveCount(0)
      expect(new URL(page.url()).pathname).toBe(url)
    }
  })

  test('a malformed workloads segment is reported explicitly', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    for (const tail of ['workloads/..%2Fsecret', 'workloads/', 'workloads/' + 'a'.repeat(200)]) {
      await page.goto(`${workspaceUrl}/${tail}`)
      await expect(page.getByRole('alert').filter({ hasText: 'not found, or you do not have access' })).toBeVisible()
      await expect(page.getByRole('tab', { name: 'Tab Overview', exact: true })).toHaveAttribute('aria-selected', 'true')
      await expect(page.locator('iframe,.xterm')).toHaveCount(0)
    }
  })

  for (const width of [1360, 390]) for (const theme of ['light', 'dark']) test(`focus mode shows only the workload with a way back to the full Console ${theme} ${width}`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 900 })
    await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
    const diagnostics: string[] = []
    page.on('console', message => { if (message.type() === 'error') diagnostics.push(message.text()) })
    page.on('pageerror', error => diagnostics.push(error.stack ?? error.message))
    await page.addInitScript(() => {
      const events: unknown[] = []
      ;(window as any).__navigation = events
      for (const method of ['pushState', 'replaceState'] as const) {
        const original = history[method].bind(history)
        history[method] = (...args: Parameters<History[typeof method]>) => {
          events.push({ method, url: args[2], at: performance.now(), stack: new Error().stack })
          return original(...args)
        }
      }
    })
    await fixture(page)
    await page.goto(`${workspaceUrl}/workloads/tab-1?focus=1`)
    await expect(page.locator('#workspace-panel-tab-1 iframe')).toBeVisible()
    await expect(page.getByRole('tablist')).toHaveCount(0)
    await expect(page.getByRole('tree')).toHaveCount(0)
    const consoleNavigation = width === 390 ? page.getByRole('button', { name: 'Browse tree', exact: true }) : page.getByRole('tablist')
    const back = page.getByRole('link', { name: 'Open full Console' })
    await expect(back).toHaveAttribute('href', `${workspaceUrl}/workloads/tab-1`)
    await page.screenshot({ path: info.outputPath(`focus-${theme}-${width}.png`) })
    const boot = await page.locator('#workspace-panel-tab-1 iframe').evaluate((iframe: HTMLIFrameElement) => (iframe.contentWindow as any).bootId)
    await back.click()
    await expect(page).toHaveURL(new RegExp(`${workspaceUrl}/workloads/tab-1$`))
    try {
      await expect(consoleNavigation).toBeVisible()
      if (width !== 390) await expect(page.getByRole('tree')).toBeVisible()
      await page.screenshot({ path: info.outputPath(`full-console-${theme}-${width}.png`) })
      // The URL must stay out of focus mode after the full Console commits;
      // a transient URL-only success used to hide the stale-effect overwrite.
      await expect(page).toHaveURL(new RegExp(`${workspaceUrl}/workloads/tab-1$`))
      expect(await page.locator('#workspace-panel-tab-1 iframe').evaluate((iframe: HTMLIFrameElement) => (iframe.contentWindow as any).bootId)).toBe(boot)
      await page.goBack()
      await expect(page.getByRole('link', { name: 'Open full Console' })).toBeVisible()
      await expect(consoleNavigation).toHaveCount(0)
      await page.getByRole('link', { name: 'Open full Console' }).press('Enter')
      await expect(consoleNavigation).toBeVisible()
      await expect(page).toHaveURL(new RegExp(`${workspaceUrl}/workloads/tab-1$`))
      // Initialization must never erase the deep-linked target, even briefly:
      // that stale replace/repair pair could later restore the old focus query.
      const erasedTargets = await page.evaluate((workspaceUrl) => ((window as any).__navigation as { url?: string }[])
        .filter(({ url }) => url && new URL(url, location.href).pathname === workspaceUrl), workspaceUrl)
      expect(erasedTargets).toEqual([])
    } catch (error) {
      console.log(JSON.stringify({ diagnostics, url: page.url(), navigation: await page.evaluate(() => (window as any).__navigation) }))
      throw error
    }
  })

  for (const mobile of [false, true]) test(`the tab menu opens the workload in a new browser tab ${mobile ? '390' : 'desktop'}`, async ({ page, context }) => {
    await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
    await fixture(page)
    await gotoWorkspace(page)
    await open(page, 'Alpha', mobile)
    if (mobile) { await page.getByRole('button', { name: /^More actions/ }).first().click() }
    else await page.getByRole('tab', { name: 'Tab /Alpha', exact: true }).click({ button: 'right' })
    const [popup] = await Promise.all([context.waitForEvent('page'), page.getByRole('menuitem', { name: 'Open in new browser tab' }).click()])
    await expect.poll(() => new URL(popup.url()).pathname).toBe('/api/workspaces/map-workspace/agents/tab-1/interface/ui/')
    expect(popup.url()).not.toMatch(/token|key|secret/i)
    await popup.close()
  })

  test('the new-tab menu item is localized', async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem('treer-lang', 'zh'))
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page); await page.goto('/')
    await page.getByRole('link', { name: '打开 /Alpha', exact: true }).click()
    await page.getByRole('tab', { name: '标签页 /Alpha', exact: true }).click({ button: 'right' })
    await expect(page.getByRole('menuitem', { name: '在新浏览器标签页中打开' })).toBeVisible()
  })
})

test.describe('workload rows are real links: modifier-click and middle-click open a new browser tab natively', () => {
  const expected = '/api/workspaces/map-workspace/agents/tab-1/interface/ui/'
  for (const how of ['ctrl', 'middle'] as const) {
    test(`overview rows and tree rows have the deep-link href and open natively (${how})`, async ({ page, context }) => {
      await page.setViewportSize({ width: 1360, height: 900 })
      await fixture(page)
      await gotoWorkspace(page)
      const click = (locator: import('@playwright/test').Locator) => how === 'ctrl' ? locator.click({ modifiers: ['ControlOrMeta'] }) : locator.click({ button: 'middle' })
      for (const target of [page.getByRole('link', { name: 'Open /Alpha', exact: true }), page.getByRole('tree').getByRole('link', { name: /^Alpha/ })]) {
        const [popup] = await Promise.all([context.waitForEvent('page'), click(target)])
        await popup.waitForURL(url => url.pathname === expected, { waitUntil: 'commit' })
        await popup.close()
        // The current page did not navigate in place.
        await expect(page.getByRole('tab', { name: 'Tab Overview', exact: true })).toHaveAttribute('aria-selected', 'true')
      }
    })
  }

  test('a plain click still opens in place', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await gotoWorkspace(page)
    await page.getByRole('link', { name: 'Open /Alpha', exact: true }).click()
    await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
  })

  test('map blocks are links; ctrl-click opens a new browser tab and keeps the map open', async ({ page, context }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await gotoWorkspace(page)
    await page.getByRole('button', { name: 'Expand tree map' }).click()
    const block = page.getByRole('dialog').getByRole('link', { name: /Alpha/ }).first()
    expect(new URL((await block.getAttribute('href'))!).pathname).toBe(expected)
    const [popup] = await Promise.all([context.waitForEvent('page'), block.click({ modifiers: ['ControlOrMeta'] })])
    await popup.waitForURL(url => url.pathname === expected, { waitUntil: 'commit' })
    await popup.close()
    await expect(page.getByRole('dialog')).toBeVisible()
  })
})

test.describe('workload links: semantics and keyboard', () => {
  test('rows are links with no nested interactive elements, and Enter and Space open in place', async ({ page }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await gotoWorkspace(page)
    const overviewLink = page.getByRole('link', { name: 'Open /Alpha', exact: true })
    const treeLink = page.getByRole('tree').getByRole('link', { name: /^Alpha/ })
    for (const link of [overviewLink, treeLink]) {
      expect(new URL((await link.getAttribute('href'))!).pathname).toBe('/api/workspaces/map-workspace/agents/tab-1/interface/ui/')
      await expect(link.locator('a,button,input,select,textarea,[tabindex],[role=button],[role=link]')).toHaveCount(0)
    }
    // The row's own actions button is a sibling of the link, not inside it.
    await expect(page.getByRole('tree').getByRole('button', { name: 'Actions for Alpha' })).toBeVisible()
    await overviewLink.focus()
    await page.keyboard.press('Enter')
    await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
    await switchTab(page, 'Overview')
    await treeLink.focus()
    const scrollBefore = await page.evaluate(() => window.scrollY)
    await page.keyboard.press('Space')
    await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
    expect(await page.evaluate(() => window.scrollY)).toBe(scrollBefore)
  })

  test('a plain click opens in place without a page load or new window', async ({ page, context }) => {
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    await gotoWorkspace(page)
    let pages = context.pages().length
    let loads = 0
    page.on('framenavigated', (frame) => { if (frame === page.mainFrame()) loads += 1 })
    const before = loads
    await page.getByRole('tree').getByRole('link', { name: /^Alpha/ }).click()
    await expect(page.getByRole('tab', { name: 'Tab /Alpha', exact: true })).toHaveAttribute('aria-selected', 'true')
    expect(context.pages().length).toBe(pages)
    // History navigation events from the in-app URL update are allowed; a document load is not.
    expect(await page.evaluate(() => performance.getEntriesByType('navigation').length)).toBe(1)
    expect(loads - before).toBeLessThanOrEqual(1)
  })
})

test.describe('an App opens on its own ingress URL in a new browser tab', () => {
  const own = 'https://svc-abc123.apps.example.test/'
  const deepLink = `${workspaceUrl}/workloads/tab-7`
  const referers: (string | undefined)[] = []
  async function withAppUrl(page: Page, publicUrl: string | null | undefined) {
    // The App's own origin is not a real host in tests; serve a stub so the new page keeps the requested URL.
    await page.context().route(/^https:\/\/svc-abc123\.apps\.example\.test\//, (route) => { referers.push(route.request().headers()['referer']); return route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>App</title><h1>App</h1>' }) })
    await page.setViewportSize({ width: 1360, height: 900 })
    await fixture(page)
    // Registered after the fixture so it takes precedence over its apps mock.
    const app = { app_id: 'app-archive', runtime_agent_id: 'tab-7', name: 'Archive', server_id: 'local', status: 'running', hostname: 'archive.test', ...(publicUrl === undefined ? {} : { public_url: publicUrl }) }
    await page.route('**/apps', (route) => route.fulfill({ json: { apps: [app] } }))
    await gotoWorkspace(page)
  }

  test('the menu item, the Overview row and the Tree row use the App URL; a plain click stays in the Console', async ({ page, context }) => {
    await withAppUrl(page, own)
    await expect(page.getByRole('link', { name: 'Open /Archive', exact: true })).toHaveAttribute('href', own)
    await expect(page.getByRole('tree').getByRole('link', { name: /^Archive/ })).toHaveAttribute('href', own)
    // External links never leak the Console as referrer or opener.
    await expect(page.getByRole('link', { name: 'Open /Archive', exact: true })).toHaveAttribute('rel', 'noopener noreferrer')
    await page.getByRole('link', { name: 'Open /Archive', exact: true }).click()
    await expect(page.getByRole('tab', { name: 'Tab /Archive', exact: true })).toHaveAttribute('aria-selected', 'true')
    await page.getByRole('tab', { name: 'Tab /Archive', exact: true }).click({ button: 'right' })
    const [popup] = await Promise.all([context.waitForEvent('page'), page.getByRole('menuitem', { name: 'Open in new browser tab' }).click()])
    await expect.poll(() => popup.url()).toBe(own)
    // The new page gets no Referer and no opener from the Console.
    await expect.poll(() => referers.length).toBe(1)
    expect(referers[0]).toBeUndefined()
    expect(await popup.evaluate(() => window.opener)).toBeNull()
    await popup.close()
  })

  test('the Tree right-click menu opens an App on its own URL', async ({ page, context }) => {
    await withAppUrl(page, own)
    await page.getByRole('tree').getByRole('link', { name: /^Archive/ }).click({ button: 'right' })
    const [popup] = await Promise.all([context.waitForEvent('page'), page.getByRole('menuitem', { name: 'Open in new browser tab' }).click()])
    await expect.poll(() => popup.url()).toBe(own)
    await popup.close()
  })

  test('Agents with UI open the fixed standalone Proxy route', async ({ page, context }) => {
    await withAppUrl(page, own)
    expect(new URL((await page.getByRole('link', { name: 'Open /Alpha', exact: true }).getAttribute('href'))!).pathname).toBe('/api/workspaces/map-workspace/agents/tab-1/interface/ui/')
    await expect(page.getByRole('link', { name: 'Open /Alpha', exact: true })).toHaveAttribute('rel', 'noopener noreferrer')
    await open(page, 'Alpha')
    await page.getByRole('tab', { name: 'Tab /Alpha', exact: true }).click({ button: 'right' })
    const [popup] = await Promise.all([context.waitForEvent('page'), page.getByRole('menuitem', { name: 'Open in new browser tab' }).click()])
    await expect.poll(() => new URL(popup.url()).pathname).toBe('/api/workspaces/map-workspace/agents/tab-1/interface/ui/')
    await popup.close()
  })

  test('an Agent without UI keeps the focus-mode Console fallback', async ({ page, context }) => {
    await withAppUrl(page, own)
    await expect(page.getByRole('link', { name: 'Open /Terminal', exact: true })).toHaveAttribute('href', `${workspaceUrl}/workloads/tab-0`)
    await open(page, 'Terminal')
    await page.getByRole('tab', { name: 'Tab /Terminal', exact: true }).click({ button: 'right' })
    const [popup] = await Promise.all([context.waitForEvent('page'), page.getByRole('menuitem', { name: 'Open in new browser tab' }).click()])
    await expect.poll(() => new URL(popup.url()).pathname + new URL(popup.url()).search).toBe(`${workspaceUrl}/workloads/tab-0?focus=1`)
    expect(await popup.evaluate(() => window.opener)).toBeNull()
    await popup.close()
  })

  for (const [label, value] of [['absent', undefined], ['null', null], ['http', 'http://svc-abc.apps.example.test/'], ['credentials', 'https://user:pw@svc-abc.apps.example.test/'], ['javascript', 'javascript:alert(1)'], ['localhost', 'https://localhost/'], ['ip literal', 'https://10.0.0.5/'], ['single label', 'https://intranet/'], ['not a URL', 'svc-abc'], ['empty', '']] as const) {
    test(`an unusable App URL (${label}) falls back to the Console deep link`, async ({ page, context }) => {
      await withAppUrl(page, value)
      await expect(page.getByRole('link', { name: 'Open /Archive', exact: true })).toHaveAttribute('href', deepLink)
      await page.getByRole('tab', { name: 'Tab Overview', exact: true }).click().catch(() => undefined)
      await page.getByRole('link', { name: 'Open /Archive', exact: true }).click()
      await page.getByRole('tab', { name: 'Tab /Archive', exact: true }).click({ button: 'right' })
      const [popup] = await Promise.all([context.waitForEvent('page'), page.getByRole('menuitem', { name: 'Open in new browser tab' }).click()])
      await expect.poll(() => new URL(popup.url()).pathname + new URL(popup.url()).search).toBe(`${deepLink}?focus=1`)
      await popup.close()
    })
  }
})
