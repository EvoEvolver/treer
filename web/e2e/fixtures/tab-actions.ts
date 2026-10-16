import { expect, type Page } from '@playwright/test'
export async function openTabMenu(page: Page, tab?: string) {
  const desktop = tab ? page.getByRole('tab', { name: `Tab ${tab}`, exact: true }) : page.getByRole('tab', { selected: true })
  if (await desktop.isVisible()) await desktop.click({ button: 'right' })
  else if (!tab) await page.getByRole('button', { name: /^More actions for / }).click()
  else {
    const origin = tab ? page.getByRole('button', { name: `Open tab ${tab}`, exact: true }) : page.getByRole('button', { name: /^Page .*; long-press for actions$/ })
    await origin.dispatchEvent('contextmenu')
  }
  await expect(page.getByRole('menu', { name: 'Page actions' })).toBeVisible()
}
export async function tabAction(page: Page, action: string, tab?: string) {
  await openTabMenu(page, tab)
  await page.getByRole('menuitem', { name: action, exact: true }).click()
}
export async function backToWorkloads(page: Page) { await tabAction(page, 'Back to workloads') }
