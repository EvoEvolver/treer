import type { Page } from '@playwright/test'

// Legacy Tree workflows explicitly enter it now that phones start at Overview.
// Trusted settings links and tour previews retain their direct destinations.
export async function browseTreeOnMobile(page: Page) {
  if ((page.viewportSize()?.width ?? 1280) >= 768 || new URL(page.url()).hash === "#policy-provider") return
  const destination = page.getByRole('button', { name: 'Browse tree', exact: true })
    .or(page.locator('#tree-sidebar:visible'))
    .or(page.getByRole('button', { name: 'Back', exact: true }))
  await destination.first().waitFor({ state: 'visible' })
  const browse = page.getByRole('button', { name: 'Browse tree', exact: true })
  if (await browse.isVisible()) await browse.click()
}

// The map belongs to Overview, independently of the mobile Tree's visibility.
export async function openOverviewMap(page: Page) {
  const overview = page.getByRole('button', { name: 'Workspace overview', exact: true })
  if (await overview.isVisible()) await overview.click()
  await page.getByRole('heading', { name: 'Workspace overview', exact: true }).waitFor({ state: 'visible' })
  await page.getByRole('button', { name: 'Expand tree map', exact: true }).click()
}
