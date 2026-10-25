import { test, expect, type Page } from '@playwright/test'
import { preparedClaudeArgs, preparedClaudeBinding } from '../src/lib/prepared-claude'

const uuid = '00112233-4455-4677-8899-aabbccddeeff'
const root = '/srv/fixture'
const machine = { server_id: 'srv-prepared', name: 'Prepared machine', hostname: 'fixture', root, status: 'online', controller_build: { version: '1', git_commit: 'fixture' }, host_build: { version: '1', git_commit: 'fixture' }, available_agents: [] }
async function fixture(page: Page, fail: boolean) {
  let writes = 0
  let payload: any
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/events$/, () => {})
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (v: unknown, status = 200) => route.fulfill({ status, json: v })
    if (path === '/api/auth/me') return json({ user_id: 'u-prepared', email: 'synthetic@example.test', preferred_name: 'Synthetic' })
    if (path === '/api/auth/config') return json({ github: false, google: false, invitation_required: true })
    if (path === '/api/invitations/pending') return route.fulfill({ status: 204 })
    if (path === '/api/organizations') return json({ organizations: [{ organization_id: 'org-prepared', name: 'Fixture', role: 'owner' }] })
    if (path === '/api/workspaces') return json({ workspaces: [{ workspace_id: 'ws-prepared', name: 'Prepared workspace' }] })
    if (path.endsWith('/snapshot')) return json({ revision: 1, workspace: { workspace_id: 'ws-prepared', name: 'Prepared workspace' }, servers: [machine], agents: [] })
    if (path.endsWith('/workload-tree')) return json({ folders: [], placements: [] })
    if (path.endsWith('/access')) return json({ access: { workspace_id: 'ws-prepared', access_mode: 'organization', current_role: 'owner', members: [], groups: [] } })
    if (path.endsWith('/members')) return json({ members: [] })
    if (path.endsWith('/groups')) return json({ groups: [] })
    if (path.endsWith('/launch-profiles')) return json({ profiles: [] })
    if (path === '/api/workspaces/ws-prepared/agents' && route.request().method() === 'POST') {
      writes++; payload = route.request().postDataJSON()
      if (fail) return json({ error: { code: 'unknown', message: 'Creation outcome unknown' } }, 503)
      return json({ agent_id: 'ag-prepared', server_id: machine.server_id, workspace_id: 'ws-prepared', name: 'Prepared Claude', kind: 'command', status: 'starting' })
    }
    return json({ services: [], hosts: [], traffic: [], apps: [], profiles: [], events: [], members: [], groups: [] })
  })
  return { writes: () => writes, payload: () => payload }
}

test('prepared argv binds machine/root/plan and rejects unacknowledged or escaped paths', () => {
  const v = { entry: '/opt/trusted/treer-agent', directory: `${root}/new target`, sessionId: uuid, acknowledgedFor: '' }
  const ready = { ...v, acknowledgedFor: preparedClaudeBinding(machine.server_id, root, 'new target', v) }
  expect(preparedClaudeArgs(machine.server_id, root, 'new target', ready)).toEqual([v.entry, 'session', 'run', '--directory', v.directory, '--session-id', uuid, '--auth', 'environment'])
  for (const [cwd, changed] of [['../escape', ready], ['new target', v], ['new target', { ...ready, entry: '/other/entry' }], ['new target', { ...ready, sessionId: 'not-uuid' }], ['new target', { ...ready, directory: `${root}/../escape` }]] as const) expect(preparedClaudeArgs(machine.server_id, root, cwd, changed)).toBeNull()
  expect(preparedClaudeArgs('srv-other', root, 'new target', ready)).toBeNull()
})

for (const width of [1280, 390]) for (const fail of [false, true]) test(`prepared Create uses ordinary endpoint once ${width} ${fail ? 'unknown' : 'success'}`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 900 })
  const fx = await fixture(page, fail)
  await page.goto('/orgs/org-prepared/workspaces/ws-prepared')
  await page.getByRole('button', { name: 'Add agent', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('combobox', { name: 'Launch' }).click()
  await page.getByRole('option', { name: 'Prepared Claude target', exact: true }).click()
  await dialog.getByLabel('Working directory', { exact: true }).fill('new target')
  await dialog.getByLabel('Trusted launcher entry (absolute path)').fill('/opt/trusted/treer-agent')
  await dialog.getByLabel('Prepared directory (absolute path)').fill(`${root}/new target`)
  await dialog.getByLabel('Prepared plan session UUID').fill(uuid)
  const submit = dialog.getByRole('button', { name: 'Create agent', exact: true })
  await expect(submit).toBeDisabled()
  await dialog.getByRole('checkbox').check()
  await expect(submit).toBeEnabled()
  // Any target edit invalidates the explicit confirmation.
  await dialog.getByLabel('Prepared plan session UUID').fill('00112233-4455-4677-8899-aabbccddee00')
  await expect(dialog.getByRole('checkbox')).not.toBeChecked()
  await expect(submit).toBeDisabled()
  await dialog.getByRole('checkbox').check()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: info.outputPath(`prepared-${width}.png`) })
  await submit.click()
  await expect.poll(fx.writes).toBe(1)
  expect(fx.payload()).toMatchObject({ server_id: machine.server_id, kind: 'command', cwd: 'new target', args: ['/opt/trusted/treer-agent', 'session', 'run', '--directory', `${root}/new target`, '--session-id', '00112233-4455-4677-8899-aabbccddee00', '--auth', 'environment'] })
  if (fail) {
    await expect(dialog.getByRole('alert')).toContainText('Creation outcome unknown')
    await expect(submit).toBeDisabled()
    await dialog.getByRole('textbox', { name: 'Name', exact: true }).press('Enter')
    expect(fx.writes()).toBe(1)
  } else {
    await expect(dialog).not.toBeVisible()
    await expect(page.getByText('Agent created and startup saved. Provider and interface readiness are not yet verified.', { exact: true })).toBeVisible()
  }
})

for (const width of [1280, 390]) test(`prepared Chinese dark keyboard ${width}`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 900 })
  await fixture(page, false)
  await page.addInitScript(() => { localStorage.setItem('treer-lang', 'zh'); localStorage.setItem('treer-theme', 'dark') })
  await page.goto('/orgs/org-prepared/workspaces/ws-prepared')
  await page.getByRole('button', { name: '添加 Agent', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('combobox', { name: '启动' }).click()
  await page.getByRole('option', { name: '已准备的 Claude 目标', exact: true }).click()
  await dialog.getByLabel('工作目录', { exact: true }).fill('new target')
  await dialog.getByLabel('可信 launcher 入口（绝对路径）').fill('/opt/trusted/treer-agent')
  await dialog.getByLabel('已准备目录（绝对路径）').fill(`${root}/new target`)
  await dialog.getByLabel('准备计划中的会话 UUID').fill(uuid)
  const ack = dialog.getByRole('checkbox')
  await ack.focus(); await page.keyboard.press('Space')
  await expect(ack).toBeChecked()
  await expect(dialog.getByRole('button', { name: '创建 Agent', exact: true })).toBeEnabled()
  expect((await ack.locator('..').boundingBox())!.height).toBeGreaterThanOrEqual(44)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: info.outputPath(`prepared-zh-dark-${width}.png`) })
  await page.keyboard.press('Escape')
  await expect(dialog).not.toBeVisible()
})
