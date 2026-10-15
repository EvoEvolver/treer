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
