import { test, expect } from '@playwright/test'
import { en } from '../src/i18n/en'
import { zh } from '../src/i18n/zh'
import { mockTreeMapApi } from './fixtures/treemap'

const placeholders = (text: string) => [...text.matchAll(/\{(\w+)\}/g)].map((match) => match[1]).sort()

test('zh defines exactly the English keys with the same placeholders and no empty text', () => {
  expect(Object.keys(zh).sort()).toEqual(Object.keys(en).sort())
  for (const key of Object.keys(en) as (keyof typeof en)[]) {
    expect(zh[key].trim(), key).not.toBe('')
    expect(placeholders(zh[key]), key).toEqual(placeholders(en[key]))
  }
})

test('HTML tour messages keep the same tags in both languages', () => {
  const tags = (text: string) => [...text.matchAll(/<\/?\w+[^>]*>/g)].map((match) => match[0])
  for (const key of Object.keys(en) as (keyof typeof en)[]) if (en[key].includes('<')) expect(tags(zh[key]), key).toEqual(tags(en[key]))
})

async function signedOut(page: import('@playwright/test').Page) {
  await page.route('**/api/auth/me', (route) => route.fulfill({ status: 401, json: { error: { message: 'Unauthorized' } } }))
  await page.route('**/api/auth/config', (route) => route.fulfill({ json: { github: false, google: false, invitation_required: false } }))
}

test.describe('browser language zh-CN', () => {
  test.use({ locale: 'zh-CN' })
  test('follows the browser, sets the document language and can switch back', async ({ page }) => {
    await signedOut(page)
    await page.goto('/')
    await expect(page.getByRole('heading', { name: '登录 Treer' })).toBeVisible()
    await expect(page.locator('html')).toHaveAttribute('lang', 'zh-Hans')
    await expect(page.getByLabel('邮箱')).toBeVisible()
    await page.getByRole('group', { name: '语言' }).getByRole('button', { name: 'English' }).click()
    await expect(page.getByRole('heading', { name: 'Sign in to Treer' })).toBeVisible()
    await expect(page.locator('html')).toHaveAttribute('lang', 'en')
    expect(await page.evaluate(() => localStorage.getItem('treer-lang'))).toBe('en')
    await page.reload()
    await expect(page.getByRole('heading', { name: 'Sign in to Treer' })).toBeVisible()
  })
})

test('English browser: stored zh wins, invalid stored value falls back, switching persists', async ({ page }) => {
  await signedOut(page)
  await page.addInitScript(() => { if (!sessionStorage.getItem('seeded')) { localStorage.setItem('treer-lang', 'zh'); sessionStorage.setItem('seeded', '1') } })
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '登录 Treer' })).toBeVisible()
  await page.evaluate(() => localStorage.setItem('treer-lang', '<script>'))
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Sign in to Treer' })).toBeVisible()
  await page.getByRole('group', { name: 'Language' }).getByRole('button', { name: '简体中文' }).click()
  await expect(page.getByRole('heading', { name: '登录 Treer' })).toBeVisible()
  await page.reload()
  await expect(page.getByRole('heading', { name: '登录 Treer' })).toBeVisible()
})

test('Settings language switch re-renders the open dialog, the menu and the embed language', async ({ page }) => {
  await mockTreeMapApi(page)
  await page.goto('/')
  await page.getByRole('button', { name: 'User menu' }).click()
  await page.getByRole('menuitem', { name: 'Settings' }).click()
  const dialog = page.getByRole('dialog', { name: 'Settings' })
  await dialog.getByRole('navigation', { name: 'Settings' }).getByRole('button', { name: 'General' }).click()
  await dialog.getByLabel('Language').click()
  await page.getByRole('option', { name: '简体中文' }).click()
  await expect(page.getByRole('dialog', { name: '设置' })).toBeVisible()
  await expect(page.getByRole('group', { name: '主题' })).toBeVisible()
  await expect(page.getByRole('button', { name: '深色' })).toBeVisible()
  await expect(page.locator('html')).toHaveAttribute('lang', 'zh-Hans')
  await page.keyboard.press('Escape')
  await page.getByRole('button', { name: '用户菜单' }).click()
  await expect(page.getByRole('menuitem', { name: '退出登录' })).toBeVisible()
  await page.keyboard.press('Escape')
  await page.reload()
  await expect(page.getByRole('button', { name: '用户菜单' })).toBeVisible()
})

test('Settings dialog sections render in zh', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('treer-lang', 'zh'))
  await mockTreeMapApi(page)
  await page.goto('/')
  await page.getByRole('button', { name: '用户菜单' }).click()
  await page.getByRole('menuitem', { name: '设置' }).click()
  const dialog = page.getByRole('dialog', { name: '设置' })
  await expect(dialog.getByRole('heading', { name: '账号' })).toBeVisible()
  await dialog.getByRole('navigation', { name: '设置' }).getByRole('button', { name: '用量与计费' }).click()
  await expect(dialog.getByText('用量与计费暂不可用')).toBeVisible()
  await dialog.getByRole('navigation', { name: '设置' }).getByRole('button', { name: '通用' }).click()
  await expect(dialog.getByRole('button', { name: '浅色' })).toBeVisible()
  await expect(dialog.getByLabel('语言')).toHaveText('简体中文')
})
