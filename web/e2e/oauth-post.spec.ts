import { test, expect, type Page } from '@playwright/test'

async function signedOut(page: Page) {
  await page.route('**/api/**', (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/auth/config') return route.fulfill({ json: { github: true, google: true, invitation_required: true } })
    if (path === '/api/invitations/pending') return route.fulfill({ status: 204 })
    return route.fulfill({ status: 401, json: { error: { message: 'authentication required' } } })
  })
}
for (const provider of ['github', 'google'] as const) test(`ordinary ${provider} sign-in starts with POST and validates the redirect`, async ({ page }) => {
  await signedOut(page)
  const origin = provider === 'github' ? 'https://github.com' : 'https://accounts.google.com'
  const destination = `${origin}/synthetic-oauth?state=synthetic`
  await page.route(`${origin}/**`, (route) => route.fulfill({ contentType: 'text/html', body: '<h1>Local intercepted provider</h1>' }))
  const methods: string[] = []
  await page.route(`**/api/auth/oauth/${provider}/start`, (route) => {
    methods.push(route.request().method())
    expect(route.request().postDataJSON()).toEqual({})
    expect(new URL(route.request().url()).search).toBe('')
    expect(route.request().headers()['x-treer-console']).toBe('1')
    expect(route.request().headers()['x-treer-client']).toBeUndefined()
    return route.fulfill({ json: { authorization_url: destination } })
  })
  await page.goto('/')
  await page.getByRole('button', { name: provider === 'github' ? 'Continue with GitHub' : 'Continue with Google', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Local intercepted provider' })).toBeVisible()
  expect(page.url()).toBe(destination)
  expect(methods).toEqual(['POST'])
})
for (const mode of ['old-proxy', 'rejected', 'unsafe-redirect'] as const) test(`OAuth ${mode} fails visibly without GET fallback`, async ({ page }) => {
  await signedOut(page)
  const methods: string[] = []
  await page.route('**/api/auth/oauth/github/start', (route) => {
    methods.push(route.request().method())
    return route.fulfill({ status: mode === 'old-proxy' ? 405 : mode === 'rejected' ? 403 : 200, json: mode === 'unsafe-redirect' ? { authorization_url: 'https://unapproved.example.test/' } : { error: { message: 'synthetic-private-error-must-not-display' } } })
  })
  await page.goto('/')
  await page.getByRole('button', { name: 'Continue with GitHub', exact: true }).click()
  await expect(page.getByText('OAuth sign-in failed. Try again.')).toBeVisible()
  await expect(page.getByText('synthetic-private-error-must-not-display')).toHaveCount(0)
  expect(new URL(page.url()).pathname).toBe('/')
  expect(methods).toEqual(['POST'])
})

test('OAuth start disables duplicate submissions until the POST completes', async ({ page }) => {
  await signedOut(page)
  let release: (() => void) | undefined, requests = 0
  await page.route('**/api/auth/oauth/github/start', async (route) => {
    requests++
    await new Promise<void>((resolve) => { release = resolve })
    await route.fulfill({ status: 403, json: {} })
  })
  await page.goto('/')
  const github = page.getByRole('button', { name: 'Continue with GitHub', exact: true })
  await github.click()
  await expect.poll(() => typeof release).toBe('function')
  await expect(github).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Continue with Google', exact: true })).toBeDisabled()
  release!()
  await expect(github).toBeEnabled()
  expect(requests).toBe(1)
})
