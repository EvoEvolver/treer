import type { Locator } from '@playwright/test'

// Concealed Tree buttons intentionally do not receive pointer events. Move onto
// the painted parent row first, as a user does, instead of forcing a hidden click.
export async function clickTreeActions(button: Locator) {
  if (await button.getAttribute('data-tree-actions') !== null) await button.locator('..').hover()
  await button.click()
}
