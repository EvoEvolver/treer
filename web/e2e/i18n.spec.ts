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

test('Preferences language switch re-renders the settings page, the menus and persists', async ({ page }) => {
  await mockTreeMapApi(page)
  await page.goto('/')
  await page.getByRole('link', { name: 'Settings', exact: true }).click()
  await page.getByRole('navigation', { name: 'Settings' }).getByRole('link', { name: 'Preferences' }).click()
  await expect(page).toHaveURL(/\/settings\/preferences$/)
  await page.getByLabel('Language').click()
  await page.getByRole('option', { name: '简体中文' }).click()
  await expect(page.getByRole('heading', { name: '偏好设置' })).toBeVisible()
  await expect(page.getByRole('group', { name: '主题' })).toBeVisible()
  await expect(page.getByRole('button', { name: '深色' })).toBeVisible()
  await expect(page.locator('html')).toHaveAttribute('lang', 'zh-Hans')
  await page.getByRole('button', { name: '用户菜单' }).click()
  await expect(page.getByRole('menuitem', { name: '退出登录' })).toBeVisible()
  await page.keyboard.press('Escape')
  await page.reload()
  await expect(page.getByRole('heading', { name: '偏好设置' })).toBeVisible()
  await expect(page.getByRole('button', { name: '用户菜单' })).toBeVisible()
})

test('Settings pages render in zh', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('treer-lang', 'zh'))
  await mockTreeMapApi(page)
  await page.goto('/settings/profile')
  const nav = page.getByRole('navigation', { name: '设置' })
  await expect(page.getByRole('heading', { name: '个人资料' })).toBeVisible()
  for (const name of ['登录与会话', '偏好设置', '成员', '访问权限', '策略']) await expect(nav.getByRole('link', { name, exact: true }).first()).toBeVisible()
  await nav.getByRole('link', { name: '偏好设置' }).click()
  await expect(page.getByRole('button', { name: '浅色' })).toBeVisible()
  await expect(page.getByLabel('语言')).toHaveText('简体中文')
  await expect(page.getByText('对应的 API')).toBeVisible()
})

test('Overview, tree and filters render in zh and the filter chip works', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('treer-lang', 'zh'))
  await mockTreeMapApi(page)
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '工作区总览' })).toBeVisible()
  await expect(page.getByRole('button', { name: '筛选:需要注意' })).toBeVisible()
  await expect(page.getByText('台机器在线')).toBeVisible()
  await expect(page.getByRole('tree', { name: '工作区工作负载' })).toBeVisible()
  await page.getByRole('button', { name: '筛选:需要注意' }).click()
  await expect(page.getByRole('button', { name: '筛选:需要注意' })).toHaveAttribute('aria-pressed', 'true')
  const body = await page.locator('main').innerText()
  for (const english of ['need attention', 'machines online', 'Workspace overview', 'Add agent']) expect(body).not.toContain(english)
})
