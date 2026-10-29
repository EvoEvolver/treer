import { test, expect, type Page, type WebSocketRoute } from '@playwright/test'
import { mapSnapshot, mockTreeMapApi } from './fixtures/treemap'

const build = { version: 'test', git_commit: 'test' }
// literature is blocked on the online workstation; reviewer-b sits on the offline build-node.
const agents = mapSnapshot.agents.map((agent) => agent.agent_id === 'researcher' ? { ...agent, status: 'blocked' } : agent)
const snapshot = { ...mapSnapshot, agents, servers: mapSnapshot.servers.map((machine) => ({ ...machine, controller_build: build, host_build: build })) }

async function fixture(page: Page) {
  await mockTreeMapApi(page)
  let events: WebSocketRoute | undefined
  await page.routeWebSocket(/\/events$/, (socket) => { events = socket })
  await page.route('**/snapshot', (route) => route.fulfill({ json: snapshot }))
  return { send: (data: unknown) => events!.send(JSON.stringify({ event: 'workspace.snapshot', data })) }
}

// The sidebar is the Tree and machines; triage lives on the Overview board, search in the top bar.
for (const theme of ['light', 'dark']) test(`sidebar tree and machines ${theme}`, async ({ page }, info) => {
  await page.setViewportSize({ width: 1360, height: 900 })
  await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
  await fixture(page)
  await page.goto('/')
  const sidebar = page.locator('#tree-sidebar')
  const tree = page.getByRole('tree', { name: 'Workspace workloads' })
  await expect(tree.getByRole('treeitem', { name: 'Research', exact: true })).toBeVisible()
  // No separate attention inbox: problems show as status icons in the Tree itself.
  await expect(sidebar.locator('[data-sidebar-section="attention"]')).toHaveCount(0)
  await expect(sidebar.getByRole('list', { name: 'Needs attention' })).toHaveCount(0)
  await page.screenshot({ path: info.outputPath(`sidebar-${theme}.png`) })

  // No filter box or Overview row: Cmd/Ctrl+K finds a workload by name or path, opens it and reveals it in the Tree.
  await expect(sidebar.getByRole('searchbox')).toHaveCount(0)
  await expect(sidebar.getByRole('button', { name: 'Workspace overview' })).toHaveCount(0)
  await page.keyboard.press('ControlOrMeta+k')
  const palette = page.getByRole('combobox', { name: 'Search the Console' })
  await expect(palette).toBeFocused()
  await palette.fill('experiments')
  await palette.press('Enter')
  await expect(tree.getByRole('treeitem', { name: 'Experiments', exact: true })).toHaveAttribute('aria-expanded', 'true')
  await expect(tree.getByRole('treeitem', { name: 'prototype', exact: true })).toHaveAttribute('aria-selected', 'true')
  await page.getByRole('tab', { name: 'Tab Overview', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Workspace overview', exact: true })).toBeVisible()

  // Machines are collapsed by default; the section remembers being opened and opens machine pages.
  const toggle = sidebar.getByRole('button', { name: /^Machines/ })
  await expect(toggle).toHaveAttribute('aria-expanded', 'false')
  await toggle.click()
  const machines = sidebar.getByRole('list', { name: 'Machines' })
  await machines.getByRole('button', { name: /^build-node 1 workloads offline$/ }).click()
  await expect(page.getByRole('heading', { name: 'build-node', exact: true })).toBeVisible()
  await machines.getByRole('button', { name: /^workstation/ }).click()
  await expect(page.getByRole('heading', { name: 'workstation', exact: true })).toBeVisible()
})
