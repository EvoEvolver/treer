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

// Daily triage from the sidebar: inbox, search, machines.
for (const theme of ['light', 'dark']) test(`sidebar inbox, search and machines ${theme}`, async ({ page }, info) => {
  await page.setViewportSize({ width: 1360, height: 900 })
  await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
  const backend = await fixture(page)
  await page.goto('/')
  const sidebar = page.locator('#tree-sidebar')
  const inbox = sidebar.getByRole('list', { name: 'Needs attention' })
  // Per-Agent problems are listed; Agents on an offline machine collapse into one machine row.
  await expect(inbox.getByRole('link')).toHaveCount(1)
  await expect(inbox.getByRole('link', { name: 'Open /Research/literature, blocked', exact: true })).toBeVisible()
  await expect(inbox.getByRole('button', { name: /^build-node offline/ })).toContainText('1 Agents down')
  await page.screenshot({ path: info.outputPath(`sidebar-${theme}.png`) })

  // An Agent that stops working while it is not open shows up as finished, until it is opened.
  backend.send({ ...snapshot, revision: 2, agents: agents.map((agent) => agent.agent_id === 'designer' ? { ...agent, status: 'idle' } : agent) })
  await expect(inbox.getByRole('link', { name: 'Open /Product/design, finished', exact: true })).toBeVisible()

  // "/" focuses the sidebar filter (Cmd/Ctrl+K opens the Console-wide search); matching rows and their folders stay.
  await page.keyboard.press('/')
  const search = sidebar.getByRole('searchbox', { name: 'Search' })
  await expect(search).toBeFocused()
  await search.fill('proto')
  const tree = page.getByRole('tree', { name: 'Workspace workloads' })
  await expect(tree.getByRole('treeitem', { name: 'prototype', exact: true })).toBeVisible()
  await expect(tree.getByRole('treeitem', { name: 'Experiments', exact: true })).toHaveAttribute('aria-expanded', 'true')
  await expect(tree.getByRole('treeitem', { name: 'design', exact: true })).toHaveCount(0)
  await search.fill('zzz-none')
  await expect(sidebar.getByText('Nothing matches “zzz-none”')).toBeVisible()
  await search.press('Escape')
  await expect(search).toHaveValue('')

  // Opening from the inbox selects the workload like the Tree does, and clears "finished".
  await inbox.getByRole('link', { name: 'Open /Product/design, finished', exact: true }).click()
  await expect(page.getByRole('tab', { name: 'Tab /Product/design', exact: true })).toHaveAttribute('aria-selected', 'true')
  await expect(inbox.getByRole('link', { name: /design/ })).toHaveCount(0)

  // The offline machine row and the Machines section both open the machine page.
  await inbox.getByRole('button', { name: /^build-node offline/ }).click()
  await expect(page.getByRole('heading', { name: 'build-node', exact: true })).toBeVisible()
  // Machines are collapsed by default; the section remembers being opened.
  const toggle = sidebar.getByRole('button', { name: /^Machines/ })
  await expect(toggle).toHaveAttribute('aria-expanded', 'false')
  await toggle.click()
  const machines = sidebar.getByRole('list', { name: 'Machines' })
  await expect(machines.getByRole('button', { name: /^build-node 1 workloads offline$/ })).toBeVisible()
  await machines.getByRole('button', { name: /^workstation/ }).click()
  await expect(page.getByRole('heading', { name: 'workstation', exact: true })).toBeVisible()
})

test('the phone shows the inbox count on its way into the sidebar', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await fixture(page)
  await page.goto('/')
  const browse = page.getByRole('button', { name: 'Browse tree', exact: true })
  await expect(browse.locator('[data-attention-badge]')).toHaveText('2')
  await browse.click()
  await expect(page.locator('#tree-sidebar').getByRole('list', { name: 'Needs attention' }).getByRole('listitem')).toHaveCount(2)
})
