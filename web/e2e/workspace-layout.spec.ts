import { test, expect } from '@playwright/test'
import { browseTreeOnMobile } from './fixtures/navigation'
import { mockTreeMapApi } from './fixtures/treemap'

for (const mobile of [false, true]) for (const theme of ['light', 'dark']) {
  test(`console composition keeps selection and creation ${theme} ${mobile ? '390' : 'desktop'}`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
    await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
    await mockTreeMapApi(page)
    const failures: string[] = []
    page.on('pageerror', (error) => failures.push(error.message))
    await page.goto('/')
    await browseTreeOnMobile(page)
    await expect(page.getByRole('tree', { name: 'Workspace workloads' })).toBeVisible()
    await expect(page.getByRole('combobox', { name: 'Workspace', exact: true })).toHaveText('Studio')
    await expect(page.getByRole('button', { name: 'Ideas', exact: true })).toBeVisible()
    if (!mobile) await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: testInfo.outputPath(`console-${theme}-${mobile ? '390' : 'desktop'}.png`) })
    await page.getByRole('button', { name: 'Add to tree', exact: true }).click()
    await page.getByRole('menuitem', { name: 'Add agent', exact: true }).click()
    const dialog = page.getByRole('dialog', { name: 'Create agent' })
    await expect(dialog.getByRole('combobox', { name: 'Machine', exact: true })).toHaveText('workstation')
    await expect(dialog.getByRole('button', { name: 'Create terminal', exact: true })).toBeEnabled()
    await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill('composition-check')
    await dialog.getByRole('button', { name: 'Cancel', exact: true }).click()
    await expect(dialog).toHaveCount(0)
    expect(failures).toEqual([])
  })
}
