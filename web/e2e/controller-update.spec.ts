import { test, expect, type Page } from '@playwright/test'
async function mount(page: Page, mode = 'normal') {
  await page.route('**/api/**', r => r.fulfill({ status: 401, json: {} }))
  await page.goto('/')
  await page.evaluate(async mode => {
    const path = '/e2e/fixtures/controller-update.tsx'; const { mountControllerUpdate } = await import(/* @vite-ignore */ path)
    ;(window as any).updateHarness = mountControllerUpdate(mode)
  }, mode)
  await page.getByRole('button', { name: 'Update Controller', exact: true }).click()
}
const dialog = (page: Page) => page.getByRole('dialog')
async function preview(page: Page) { await dialog(page).getByRole('button', { name: 'Preview 1.1.0', exact: true }).click(); await expect(dialog(page).getByRole('heading', { name: 'Review Controller update' })).toBeFocused() }
for (const mode of ['normal', 'lost-response', 'lost-unknown']) test(`one dispatch only with ${mode}; close/reopen/check never redispatch`, async ({ page }) => {
  await mount(page, mode); await preview(page)
  await dialog(page).getByRole('button', { name: 'Update Controller', exact: true }).evaluate((button: HTMLButtonElement) => { button.click(); button.click() })
  await expect(dialog(page)).toContainText(mode === 'lost-unknown' ? 'Result unconfirmed' : 'Waiting for the machine to reconnect')
  await dialog(page).getByRole('button', { name: 'Check update status', exact: true }).click()
  await page.keyboard.press('Escape')
  await expect(page.getByRole('button', { name: 'Update Controller', exact: true })).toBeFocused()
  await page.getByRole('button', { name: 'Update Controller', exact: true }).click()
  await dialog(page).getByRole('button', { name: 'Check update status', exact: true }).click()
  expect(await page.evaluate(() => (window as any).updateHarness.counts.dispatch)).toBe(1)
  expect(await page.evaluate(() => (window as any).updateHarness.counts.prepare)).toBe(1)
  await expect(dialog(page)).not.toContainText('PRIVATE')
})
for (const [mode, text] of [['unsupported', 'Update it manually first'], ['offline', 'Machine offline'], ['forbidden', 'do not have permission'], ['unavailable', 'could not be verified'], ['unverified', 'Result unconfirmed'], ['success', 'Target version and health verified'], ['rollback', 'Restored version: 1.0.0'], ['known-offline', 'Target version and health verified'], ['core-healthy', 'launcher reconciliation pending'], ['busy', 'Another machine operation'], ['partial', 'Update incomplete'], ['rollback-failed', 'Rollback failed']] as const) test(`distinct ${mode} outcome`, async ({ page }) => {
  await mount(page, mode); await expect(dialog(page)).toContainText(text)
  await expect(dialog(page).getByRole('button', { name: /Preview / })).toHaveCount(0)
  expect(await page.evaluate(() => (window as any).updateHarness.counts.dispatch)).toBe(0)
})
test('expired or wrong-machine preview cannot dispatch', async ({ page }) => {
  await mount(page, 'expired'); await preview(page)
  await expect(dialog(page).getByRole('button', { name: 'Update Controller', exact: true })).toBeDisabled()
  expect(await page.evaluate(() => (window as any).updateHarness.counts.dispatch)).toBe(0)
  await page.evaluate(() => (window as any).updateHarness.unmount())
  await mount(page, 'wrong-machine'); await dialog(page).getByRole('button', { name: 'Preview 1.1.0', exact: true }).click()
  await expect(dialog(page).getByRole('alert')).toContainText('could not be verified')
  await expect(dialog(page).getByRole('button', { name: 'Update Controller', exact: true })).toHaveCount(0)
})
test('unmount and new view recover existing durable operation by inspect only', async ({ page }) => {
  await mount(page, 'normal'); await preview(page); await dialog(page).getByRole('button', { name: 'Update Controller', exact: true }).click()
  await expect(dialog(page)).toContainText('Waiting for the machine')
  await page.evaluate(() => (window as any).updateHarness.remount())
  await page.getByRole('button', { name: 'Update Controller', exact: true }).click()
  await expect(dialog(page)).toContainText('Waiting for the machine')
  expect(await page.evaluate(() => (window as any).updateHarness.counts.dispatch)).toBe(1)
})
test('long release labels fit en/zh light/dark desktop and touch preview', async ({ page }, info) => {
  await mount(page, 'long')
  await dialog(page).getByRole('button', { name: /^Preview / }).click()
  for (const language of ['en', 'zh']) for (const dark of [false, true]) for (const width of [1360, 390]) {
    await page.evaluate(async ({ language, dark }) => { const path = '/src/i18n/index.ts'; const { setLocale } = await import(/* @vite-ignore */ path); setLocale(language, false); document.documentElement.classList.toggle('dark', dark) }, { language, dark })
    await page.setViewportSize({ width, height: 900 })
    await expect(dialog(page).getByRole('heading', { name: language === 'zh' ? '更新 Controller' : 'Update Controller', exact: true })).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    expect(await dialog(page).evaluate(e => e.scrollWidth <= e.clientWidth)).toBe(true)
    await expect(dialog(page).locator('img')).toHaveCount(0)
    if (width === 390) for (const b of await dialog(page).getByRole('button').all()) { const box = await b.boundingBox(); expect(box!.height).toBeGreaterThanOrEqual(44) }
    await page.screenshot({ path: info.outputPath(`controller-update-${language}-${dark ? 'dark' : 'light'}-${width}.png`) })
  }
  expect(await page.evaluate(() => [...Object.values(localStorage), ...Object.values(sessionStorage)].some(v => v.includes('PRIVATE-CONFIRMATION-TOKEN')))).toBe(false)
})

test('success needs whole receipt, health and all four identity fields; core health alone is insufficient', async ({ page }) => {
  await page.route('**/api/**', r => r.fulfill({ status: 401, json: {} })); await page.goto('/')
  const states = await page.evaluate(async () => {
    const path = '/src/lib/controller-update.ts'; const { controllerUpdateStatus } = await import(/* @vite-ignore */ path)
    const identity = { release_sha: 'a'.repeat(40), artifact_source_sha: 'a'.repeat(40), controller_sha256: 'a'.repeat(64), cli_sha256: 'a'.repeat(64) }
    const valid = { status: 'succeeded', target: { identity }, receiptVerified: true, phase: 'complete', health: 'passed', observedIdentity: identity }
    return [valid, { ...valid, observation: 'unavailable' }, { ...valid, receiptVerified: false }, { ...valid, phase: 'core_healthy' }, { ...valid, health: 'unknown' }, ...Object.keys(identity).map(key => ({ ...valid, observedIdentity: { ...identity, [key]: 'b'.repeat(key.endsWith('sha256') ? 64 : 40) } }))].map(controllerUpdateStatus)
  })
  expect(states).toEqual(['succeeded', 'succeeded', ...Array(7).fill('unconfirmed')])
})

test('rollback display requires restored original identity, receipt and correlated health', async ({ page }) => {
  await page.route('**/api/**', r => r.fulfill({ status: 401, json: {} })); await page.goto('/')
  const result = await page.evaluate(async () => {
    const path = '/src/lib/controller-update.ts'; const { controllerUpdateStatus } = await import(/* @vite-ignore */ path)
    const identity = { release_sha: 'a'.repeat(40), artifact_source_sha: 'a'.repeat(40), controller_sha256: 'a'.repeat(64), cli_sha256: 'a'.repeat(64) }
    const op = { status: 'rolled_back', current: { identity }, restored: { identity }, observedIdentity: identity, receiptVerified: true, phase: 'rolled_back', health: 'passed' }
    return [op, { ...op, health: 'unknown' }, { ...op, receiptVerified: false }, { ...op, restored: null }, { ...op, observedIdentity: { ...identity, cli_sha256: 'b'.repeat(64) } }].map(controllerUpdateStatus)
  })
  expect(result).toEqual(['rolled_back', ...Array(4).fill('unconfirmed')])
})
