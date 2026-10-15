import { test, expect, type Page } from '@playwright/test'
import { workerOrigin } from './ports'
import { oauthAuthorizationUrl } from '../src/lib/oauth-start'
import { mockTreeMapApi, mapSnapshot } from './fixtures/treemap'
const token = 'inv_synthetic_organization_fixture'
const invited = { organization_id: 'invited-org', name: 'Invited Studio', role: 'member' }
const user = { user_id: 'existing', email: 'existing@example.test', preferred_name: 'Existing' }
async function setup(page: Page, options: { signedIn?: boolean; kind?: 'personal' | 'organization'; status?: 'valid' | 'invalid'; unsupported?: boolean; missingCapability?: boolean; acceptError?: number; name?: string; pending?: boolean; alreadyJoined?: boolean; declineError?: boolean } = {}) {
  await mockTreeMapApi(page)
  let signedIn = options.signedIn ?? true, joined = options.alreadyJoined ?? false
  let pending = options.pending ?? false, pendingGets = 0, declines = 0
  const preview = { capabilities: options.missingCapability ? [] : ['organization.invitation-accept.v1'], kind: options.kind ?? 'organization', organization_id: options.kind === 'personal' ? null : invited.organization_id, organization_name: options.name ?? invited.name, status: options.status ?? 'valid' }
  const registrations: Record<string, unknown>[] = [], accepts: Record<string, unknown>[] = [], previews: string[] = []
  await page.route('**/api/auth/me', (route) => route.fulfill(signedIn ? { json: user } : { status: 401, json: { error: { code: 'unauthorized', message: 'Sign in' } } }))
  await page.route('**/api/auth/config', (route) => route.fulfill({ json: { github: true, google: true, invitation_required: true } }))
  await page.route('**/api/organizations', (route) => route.fulfill({ json: { organizations: [{ organization_id: 'map-org', name: 'Studio', role: 'owner' }, ...(joined ? [invited] : [])] } }))
  await page.route('**/api/workspaces?*', (route) => route.fulfill({ json: { workspaces: new URL(route.request().url()).searchParams.get('organization_id') === invited.organization_id ? [] : [mapSnapshot.workspace] } }))
  await page.route('**/api/invitations/pending', (route) => {
    if (route.request().method() === 'DELETE') {
      declines++
      if (options.declineError) return route.fulfill({ status: 503, json: {} })
      pending = false
      return route.fulfill({ status: 204 })
    }
    pendingGets++
    return route.fulfill(pending ? { json: { ...preview, pending_id: 'synthetic-pending-id' } } : { status: 204 })
  })
  await page.route('**/api/invitations/preview', (route) => {
    previews.push(route.request().url())
    expect(route.request().postDataJSON()).toEqual({ token })
    return route.fulfill(options.unsupported ? { status: 404, json: {} } : { json: preview })
  })
  await page.route('**/api/invitations/accept', (route) => {
    accepts.push(route.request().postDataJSON())
    if (options.acceptError) return route.fulfill({ status: options.acceptError, json: { error: { code: 'invalid_invitation', message: 'Synthetic rejected invite' } } })
    joined = true
    pending = false
    return route.fulfill({ json: { organization_id: invited.organization_id, role: 'member', already_member: false } })
  })
  for (const mode of ['login', 'register']) await page.route(`**/api/auth/${mode}`, (route) => {
    signedIn = true
    if (mode === 'register') { registrations.push(route.request().postDataJSON()); joined = options.kind !== 'personal' }
    return route.fulfill({ json: user })
  })
  return { registrations, accepts, previews, get pendingGets() { return pendingGets }, get declines() { return declines } }
}
async function authenticate(page: Page, register = false) {
  if (register) await page.getByRole('button', { name: 'Create account', exact: true }).click()
  await page.getByLabel('Email', { exact: true }).fill('existing@example.test')
  if (register) await page.getByLabel('Preferred name', { exact: true }).fill('Existing')
  await page.getByLabel('Password', { exact: true }).fill('synthetic-password')
  await page.getByRole('button', { name: register ? 'Create account' : 'Sign in', exact: true }).click()
}
for (const mobile of [false, true]) for (const theme of ['light', 'dark']) test(`signed-in org invitation explicit consent ${theme} ${mobile ? '390' : 'desktop'}`, async ({ page }, info) => {
  await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
  await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
  const backend = await setup(page)
  await page.goto(`/?invite=${token}&keep=1`)
  const dialog = page.getByRole('dialog', { name: 'Join Invited Studio?' })
  await expect(dialog).toBeVisible()
  await expect(dialog).toContainText('existing@example.test')
  expect(backend.accepts).toEqual([])
  await expect.poll(() => new URL(page.url()).searchParams.has('invite')).toBe(false)
  expect(new URL(page.url()).searchParams.get('keep')).toBe('1')
  expect(backend.previews.every((url) => !url.includes(token))).toBe(true)
  expect(await page.evaluate((token) => [...Object.values(localStorage), ...Object.values(sessionStorage)].some((value) => value.includes(token)), token)).toBe(false)
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: info.outputPath(`org-invite-${theme}-${mobile ? '390' : 'desktop'}.png`) })
  await page.getByRole('button', { name: 'Join organization', exact: true }).click()
  await expect(page).toHaveURL(/\/orgs\/invited-org/)
  if (mobile) await page.getByRole('button', { name: 'Browse tree', exact: true }).click()
  await expect(page.getByRole('combobox', { name: 'Organization', exact: true })).toHaveText('Invited Studio')
  expect(backend.accepts).toEqual([{ token }])
  expect(new URL(page.url()).pathname).toBe('/orgs/invited-org')
})
test('cancel never accepts and hostile organization name remains text', async ({ page }) => {
  const backend = await setup(page, { name: '<img src=x onerror=alert(1)>' })
  await page.goto(`/?invite=${token}`)
  await expect(page.getByRole('heading', { name: 'Join <img src=x onerror=alert(1)>?' })).toBeVisible()
  expect(await page.locator('img').count()).toBe(0)
  await page.getByRole('button', { name: 'Cancel', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  expect(backend.accepts).toEqual([])
})
test('signed-out existing user confirms before login and explicitly accepts afterwards', async ({ page }) => {
  const backend = await setup(page, { signedIn: false })
  await page.goto(`/?invite=${token}`)
  await expect(page.getByRole('dialog', { name: 'Join Invited Studio?' })).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Email', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'Join organization', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Sign in to Treer' })).toBeVisible()
  await authenticate(page)
  await expect(page.getByRole('dialog', { name: 'Join Invited Studio?' })).toBeVisible()
  expect(backend.accepts).toEqual([])
  await page.getByRole('button', { name: 'Join organization', exact: true }).click()
  await expect(page.getByRole('combobox', { name: 'Organization', exact: true })).toHaveText('Invited Studio')
  expect(backend.accepts).toEqual([{ token }])
})
test('new user confirms before registration; existing registration membership is not accepted twice', async ({ page }) => {
  const backend = await setup(page, { signedIn: false })
  await page.goto(`/?invite=${token}`)
  expect(backend.registrations).toEqual([])
  await page.getByRole('button', { name: 'Join organization', exact: true }).click()
  await authenticate(page, true)
  await expect(page.getByRole('combobox', { name: 'Organization', exact: true })).toHaveText('Invited Studio')
  expect(backend.registrations[0]).toMatchObject({ invite: token, join_organization: true })
  expect(backend.accepts).toEqual([])
  expect(new URL(page.url()).searchParams.has('invite')).toBe(false)
})
for (const signedIn of [true, false]) test(`invalid or consumed invitation is clear, signedIn=${signedIn}`, async ({ page }) => {
  const backend = await setup(page, { signedIn, status: 'invalid' })
  await page.goto(`/?invite=${token}`)
  await expect(page.getByRole('alert')).toContainText('invalid or no longer available')
  await expect(page.getByRole('button', { name: 'Join organization', exact: true })).toHaveCount(0)
  expect(backend.accepts).toEqual([])
  expect(new URL(page.url()).searchParams.has('invite')).toBe(false)
})
test('old Proxy fails closed without offering registration fallback', async ({ page }) => {
  const backend = await setup(page, { signedIn: false, unsupported: true })
  await page.goto(`/?invite=${token}`)
  await expect(page.getByRole('alert')).toContainText('not supported by this server')
  await expect(page.getByRole('button', { name: 'Create account', exact: true })).toHaveCount(0)
  expect(backend.accepts).toEqual([])
})
test('invitation consumed between preview and accept displays a clear error', async ({ page }) => {
  await setup(page, { acceptError: 409 })
  await page.goto(`/?invite=${token}`)
  await page.getByRole('button', { name: 'Join organization', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('invalid or no longer available')
  expect(new URL(page.url()).searchParams.has('invite')).toBe(false)
})
test('personal invitation remains registration-only and never accepts an organization', async ({ page }) => {
  const backend = await setup(page, { signedIn: false, kind: 'personal' })
  await page.goto(`/?invite=${token}`)
  await expect(page.getByRole('heading', { name: 'Join Treer' })).toBeVisible()
  await page.getByLabel('Email', { exact: true }).fill('existing@example.test')
  await page.getByLabel('Preferred name', { exact: true }).fill('Existing')
  await page.getByLabel('Password', { exact: true }).fill('synthetic-password')
  await page.getByRole('button', { name: 'Create account', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  expect(backend.registrations[0].invite).toBe(token)
  expect(backend.accepts).toEqual([])
})
test('OAuth requires consent first and carries intent only in the Core start request', async ({ page }) => {
  const backend = await setup(page, { signedIn: false })
  await page.route('**/api/auth/oauth/github/start', (route) => route.fulfill({ json: { authorization_url: 'https://unapproved.example.test/' } }))
  await page.goto(`/?invite=${token}`)
  await expect(page.getByRole('button', { name: /GitHub/ })).toHaveCount(0)
  await page.getByRole('button', { name: 'Join organization', exact: true }).click()
  const request = page.waitForRequest((req) => req.url().includes('/api/auth/oauth/github/start'))
  await page.getByRole('button', { name: /GitHub/ }).click()
  const start = await request
  expect(start.method()).toBe('POST')
  expect(new URL(start.url()).search).toBe('')
  expect(start.headers()['x-treer-console']).toBe('1')
  expect(start.postDataJSON()).toEqual({ invite_token: token, join_organization: true })
  await expect(page.getByText('OAuth sign-in failed. Try again.')).toBeVisible()
  expect(backend.accepts).toEqual([])
})
for (const mobile of [false, true]) test(`OAuth existing-account return restores cookie-bound pending, ${mobile ? '390' : 'desktop'}`, async ({ page }) => {
  await page.setViewportSize({ width: mobile ? 390 : 1360, height: 900 })
  const backend = await setup(page, { pending: true })
  await page.goto('/')
  await expect(page.getByRole('dialog', { name: 'Join Invited Studio?' })).toBeVisible()
  expect(backend.accepts).toEqual([])
  expect(backend.pendingGets).toBe(1)
  expect(page.url()).not.toContain(token)
  expect(page.url()).not.toContain('synthetic-pending-id')
  expect(await page.evaluate(() => [...Object.values(localStorage), ...Object.values(sessionStorage)].some((value) => value.includes('synthetic-pending-id')))).toBe(false)
  await page.getByRole('button', { name: 'Join organization', exact: true }).click()
  await expect(page).toHaveURL(/\/orgs\/invited-org/)
  expect(backend.accepts).toEqual([{ pending_id: 'synthetic-pending-id' }])
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await expect(page.getByRole('dialog')).toHaveCount(0)
})

test('keyboard Escape cancels without acceptance', async ({ page }) => {
  const backend = await setup(page)
  await page.goto(`/?invite=${token}`)
  await expect(page.getByRole('heading', { name: 'Join Invited Studio?' })).toBeFocused()
  await page.keyboard.press('Escape')
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  expect(backend.accepts).toEqual([])
})
test('signed-in personal invitation cannot be used to join an organization', async ({ page }) => {
  const backend = await setup(page, { kind: 'personal' })
  await page.goto(`/?invite=${token}`)
  await expect(page.getByRole('dialog')).toContainText('not joining an organization')
  await expect(page.getByRole('button', { name: 'Join organization', exact: true })).toHaveCount(0)
  expect(backend.accepts).toEqual([])
})
test('accept failure is localized and can be retried without losing intent', async ({ page }) => {
  const backend = await setup(page)
  let attempts = 0
  await page.route('**/api/invitations/accept', async (route) => {
    if (++attempts === 1) return route.fulfill({ status: 503, json: { error: { code: 'unavailable', message: 'Synthetic service unavailable' } } })
    return route.fallback()
  })
  await page.goto(`/?invite=${token}`)
  await page.getByRole('button', { name: 'Join organization', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('Unable to join')
  await page.getByRole('button', { name: 'Join organization', exact: true }).click()
  await expect(page).toHaveURL(/\/orgs\/invited-org/)
  expect(backend.accepts).toEqual([{ token }])
})
test('Chinese invitation confirmation and failure copy', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('treer-lang', 'zh'))
  await setup(page, { acceptError: 409 })
  await page.goto(`/?invite=${token}`)
  await expect(page.getByRole('dialog')).toContainText('Invited Studio')
  await page.getByRole('button', { name: '加入组织', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('邀请')
})

test('missing capability fails closed even if the preview endpoint exists', async ({ page }) => {
  const backend = await setup(page, { signedIn: false, missingCapability: true })
  await page.goto(`/?invite=${token}`)
  await expect(page.getByRole('alert')).toContainText('not supported by this server')
  await expect(page.getByRole('button', { name: 'Join organization', exact: true })).toHaveCount(0)
  await expect(page.getByRole('textbox', { name: 'Email', exact: true })).toHaveCount(0)
  expect(backend.accepts).toEqual([])
})

test('declining an OAuth pending deletes it and does not prompt again on reload', async ({ page }) => {
  const backend = await setup(page, { pending: true })
  await page.goto('/')
  await expect(page.getByRole('dialog')).toContainText('Invited Studio')
  await page.getByRole('button', { name: 'Cancel', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  expect(backend.declines).toBe(1)
  expect(backend.accepts).toEqual([])
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await expect(page.getByRole('dialog')).toHaveCount(0)
})
test('failed pending decline is visible, retryable, and never accepts', async ({ page }) => {
  const backend = await setup(page, { pending: true, declineError: true })
  await page.goto('/')
  await page.getByRole('button', { name: 'Cancel', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('Unable to dismiss')
  await expect(page.getByRole('button', { name: 'Cancel', exact: true })).toBeEnabled()
  expect(backend.accepts).toEqual([])
})
test('OAuth new account already joined in Core opens the organization without another acceptance', async ({ page }) => {
  const backend = await setup(page, { alreadyJoined: true })
  await page.goto('/orgs/invited-org')
  await expect(page.getByRole('combobox', { name: 'Organization', exact: true })).toHaveText('Invited Studio')
  expect(backend.accepts).toEqual([])
  await expect(page.getByRole('dialog')).toHaveCount(0)
})
test('old Proxy without pending support still opens the ordinary console', async ({ page }) => {
  await setup(page)
  await page.route('**/api/invitations/pending', (route) => route.fulfill({ status: 404, json: {} }))
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await expect(page.getByRole('alert')).toHaveCount(0)
})

test('built Worker permits JSON OAuth start; validated navigation is cancelled before network', async ({ page }) => {
  await setup(page, { signedIn: false })
  // Cancel a direct location.assign before any network navigation. The route
  // abort is an additional guard, not a mock of a 303 redirect chain.
  await page.addInitScript(() => {
    window.navigation.addEventListener('navigate', (event) => {
      if (new URL(event.destination.url).origin !== location.origin) {
        event.preventDefault()
        Object.assign(window, { oauthDestination: event.destination.url })
      }
    })
  })
  let externalRequest = false
  await page.route('https://github.com/**', (route) => { externalRequest = true; return route.abort() })
  let posted = false
  await page.route('**/api/auth/oauth/github/start', (route) => {
    posted = true
    expect(route.request().method()).toBe('POST')
    expect(route.request().headers().referer).toBeUndefined()
    expect(route.request().headers()['x-treer-console']).toBe('1')
    expect(new URL(route.request().url()).search).toBe('')
    expect(route.request().postDataJSON()).toEqual({ invite_token: token, join_organization: true })
    return route.fulfill({ json: { authorization_url: 'https://github.com/login/oauth/authorize?state=synthetic-state' } })
  })
  await page.goto(`${workerOrigin}/?invite=${token}`)
  await page.getByRole('button', { name: 'Join organization', exact: true }).click()
  await page.getByRole('button', { name: /GitHub/ }).click()
  await expect.poll(() => page.evaluate(() => (window as Window & { oauthDestination?: string }).oauthDestination)).toBe('https://github.com/login/oauth/authorize?state=synthetic-state')
  expect(posted).toBe(true)
  expect(externalRequest).toBe(false)
  expect(page.url()).not.toContain(token)
})
test('OAuth navigation accepts only selected HTTPS IdP URLs without invitation parameters', () => {
  expect(oauthAuthorizationUrl('github', 'https://github.com/login/oauth/authorize?state=synthetic')).toContain('state=synthetic')
  expect(oauthAuthorizationUrl('google', 'https://accounts.google.com/o/oauth2/v2/auth?state=synthetic')).toContain('state=synthetic')
  for (const url of ['https://evil.example.test/', 'http://github.com/', 'https://github.com.evil.example.test/', 'https://synthetic@github.com/', 'javascript:alert(1)', 'https://github.com/#fragment', 'https://github.com/?invite=synthetic', 'https://github.com/?invite_token=synthetic', 'https://github.com/?pending_id=synthetic', 'https://accounts.google.com/']) {
    expect(() => oauthAuthorizationUrl('github', url)).toThrow()
  }
})

test('malformed pending response shows a safe localized notice without echoing its body', async ({ page }) => {
  await setup(page)
  await page.route('**/api/invitations/pending', (route) => route.fulfill({ contentType: 'text/plain', body: 'synthetic-private-response-not-json' }))
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
  await expect(page.getByRole('alert')).toContainText('Unable to check this invitation')
  await expect(page.locator('body')).not.toContainText('synthetic-private-response-not-json')
})
