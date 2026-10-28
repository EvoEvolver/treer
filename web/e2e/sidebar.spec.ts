import { test, expect } from '@playwright/test'
import { mapSnapshot, mockTreeMapApi } from './fixtures/treemap'

const build = { version: 'test', git_commit: 'test' }

// Daily triage from the sidebar: attention inbox, search, machines; and the phone's way in.
for (const theme of ['light', 'dark']) test(`sidebar inbox, search and machines ${theme}`, async ({ page }, info) => {
  await page.setViewportSize({ width: 1360, height: 900 })
  await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
  await mockTreeMapApi(page)
  await page.route('**/snapshot', (route) => route.fulfill({ json: { ...mapSnapshot, servers: mapSnapshot.servers.map((machine) => ({ ...machine, controller_build: build, host_build: build })) } }))
  await page.goto('/')
  const sidebar = page.locator('#tree-sidebar')
  const inbox = sidebar.getByRole('list', { name: 'Needs attention' })
  // reviewer-b runs on the offline build-node, so it needs attention.
  await expect(inbox.getByRole('link')).toHaveCount(1)
  await expect(inbox.getByRole('link', { name: 'Open /Product/reviewer, offline', exact: true })).toBeVisible()
  await page.screenshot({ path: info.outputPath(`sidebar-${theme}.png`) })

  // Ctrl/Cmd+K focuses search; matching rows and their folders stay, everything else hides.
  await page.keyboard.press('ControlOrMeta+k')
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
  await expect(tree.getByRole('treeitem', { name: 'Research', exact: true })).toBeVisible()

  // Opening from the inbox selects the workload like the Tree does.
  await inbox.getByRole('link').first().click()
  await expect(page.getByRole('tab', { name: 'Tab /Product/reviewer', exact: true })).toHaveAttribute('aria-selected', 'true')

  // Machines are one click away, with their health.
  const machines = sidebar.getByRole('list', { name: 'Machines' })
  await expect(machines.getByRole('button', { name: /^build-node 1 workloads offline$/ })).toBeVisible()
  await machines.getByRole('button', { name: /^workstation/ }).click()
  await expect(page.getByRole('heading', { name: 'workstation', exact: true }).first()).toBeVisible()
})

test('the phone shows the attention count on its way into the sidebar', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await mockTreeMapApi(page)
  await page.goto('/')
  const browse = page.getByRole('button', { name: 'Browse tree', exact: true })
  await expect(browse.locator('[data-attention-badge]')).toHaveText('1')
  await browse.click()
  await expect(page.locator('#tree-sidebar').getByRole('list', { name: 'Needs attention' }).getByRole('link')).toHaveCount(1)
})
