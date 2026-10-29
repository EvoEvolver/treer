import type { Page } from '@playwright/test'

// Legacy Tree workflows explicitly enter it now that phones start at Overview.
// Trusted settings links and tour previews retain their direct destinations.
export async function browseTreeOnMobile(page: Page) {
  if ((page.viewportSize()?.width ?? 1280) >= 768 || new URL(page.url()).hash === "#policy-provider") return
  // CSS changes immediately on resize, while matchMedia/React may still render
  // the desktop sidebar. Wait for the React-only mobile button to be mounted.
  // The button reads "Close tree" once the Tree is already open.
  await page.getByRole('button', { name: /^(Browse|Close) tree$/, includeHidden: true }).waitFor({ state: 'attached' })
  const destination = page.getByRole('button', { name: 'Browse tree', exact: true })
    .or(page.locator('#tree-sidebar:visible'))
    .or(page.getByRole('button', { name: 'Back', exact: true }))
  await destination.first().waitFor({ state: 'visible' })
  const browse = page.getByRole('button', { name: 'Browse tree', exact: true })
  if (await browse.isVisible()) {
    await browse.click()
    await page.locator('#tree-sidebar').waitFor({ state: 'visible' })
  }
}

// The map belongs to Overview, independently of the mobile Tree's visibility.
export async function openOverviewMap(page: Page) {
  // Phones close the Tree above Overview; desktop selects the pinned Overview tab.
  const closeTree = page.getByRole('button', { name: 'Close tree', exact: true })
  const overviewTab = page.getByRole('tab', { name: 'Tab Overview', exact: true })
  if (await closeTree.isVisible()) await closeTree.click()
  else if (await overviewTab.isVisible()) await overviewTab.click()
  await page.getByRole('heading', { name: 'Workspace overview', exact: true }).waitFor({ state: 'visible' })
  await page.getByRole('button', { name: 'Expand tree map', exact: true }).click()
}
