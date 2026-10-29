import { test, expect } from '@playwright/test'
import { mockTreeMapApi } from './fixtures/treemap'
import { browseTreeOnMobile } from './fixtures/navigation'

test('Browse tree waits for the React mobile layout after CSS has already resized', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 })
  await mockTreeMapApi(page)
  // Model a busy renderer: CSS sees the phone width immediately, but delivery
  // of the matching React media-query listener is delayed. No product changes.
  await page.addInitScript(() => {
    const match = window.matchMedia.bind(window)
    window.matchMedia = (query) => {
      const media = match(query)
      if (query === '(max-width: 767px)') {
        const add = media.addEventListener.bind(media)
        Object.defineProperty(media, 'addEventListener', { value(type: string, listener: EventListenerOrEventListenerObject, options?: AddEventListenerOptions) {
          add(type, (event) => setTimeout(() => {
            if (typeof listener === 'function') listener.call(media, event)
            else listener.handleEvent(event)
          }, 300), options)
        } })
      }
      return media
    }
  })
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Workspace overview', exact: true })).toBeVisible()
  for (let cycle = 0; cycle < 3; cycle++) {
    await page.setViewportSize({ width: 390, height: 844 })
    await browseTreeOnMobile(page)
    // Read the committed layout at return time, rather than retrying until the
    // delayed render hides the helper's premature return.
    expect(await page.getByRole('button', { name: /^(Browse|Close) tree$/, includeHidden: true }).count()).toBe(1)
    await expect(page.locator('#tree-sidebar')).toBeVisible()
    await expect(page.getByRole('heading', { name: 'Workspace overview', exact: true })).toBeHidden()
    // Already-open Tree is also a valid completed destination.
    await browseTreeOnMobile(page)
    await expect(page.locator('#tree-sidebar')).toBeVisible()
    await page.setViewportSize({ width: 1280, height: 900 })
    await expect(page.getByRole('button', { name: /^(Browse|Close) tree$/, includeHidden: true })).toHaveCount(0)
  }
})
