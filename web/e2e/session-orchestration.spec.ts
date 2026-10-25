import { test, expect, type Page } from '@playwright/test'
import { preparedClaudeArgs, preparedClaudeBinding } from '../src/lib/prepared-claude'
import { createHash } from 'node:crypto'
const root = '/fixture/root'
const sourceId = 'ag-source'
const sourceSession = '22334455-6677-4899-aabb-ccddeeff0011'
const machine = { server_id: 'srv-prepared', name: 'Fixture machine', root, status: 'online', capabilities: ['machine.exec.v1'], controller_build: { version: 'fixture', git_commit: 'a'.repeat(40) }, host_build: { version: 'fixture', git_commit: 'b'.repeat(40) } }
const source = { agent_id: sourceId, server_id: machine.server_id, workspace_id: 'ws-prepared', name: 'Source Claude', kind: 'command', status: 'running' }
async function fixture(page: Page, failure: '' | 'upload' | 'prepare' | 'receipt' | 'instance' | 'owner-env' | 'owner-intent' | 'owner-mismatch' | 'owner-auth' | 'owner-project' | 'source-auth-missing' | 'source-auth-mismatch' | 'owner-canonical' | 'owner-key' | 'owner-canonical-change' = '') {
  let loggedIn = false, created = false, session = '', sourceStatusReads = 0
  const counts = { export: 0, upload: 0, inspect: 0, prepare: 0, create: 0 }
  let payload: any
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/events$/, () => {})
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (v: unknown, status = 200) => route.fulfill({ status, json: v })
    if (path === '/api/auth/me') return loggedIn ? json({ user_id: 'u-copy', email: 'owner@example.test', preferred_name: 'Owner' }) : json({ error: { code: 'authentication_required', message: 'Sign in' } }, 401)
    if (path === '/api/auth/login') { loggedIn = true; return json({ user_id: 'u-copy', email: 'owner@example.test', preferred_name: 'Owner' }) }
    if (path === '/api/auth/config') return json({ github: false, google: false, invitation_required: true })
    if (path === '/api/invitations/pending') return route.fulfill({ status: 204 })
    if (path === '/api/organizations') return json({ organizations: [{ organization_id: 'org-prepared', name: 'Fixture', role: 'owner' }] })
    if (path === '/api/workspaces') return json({ workspaces: [{ workspace_id: 'ws-prepared', name: 'Workspace' }] })
    if (path.endsWith('/snapshot')) return json({ revision: 1, workspace: { workspace_id: 'ws-prepared', name: 'Workspace' }, servers: [machine], agents: [source, ...(created ? [{ ...source, agent_id: 'ag-copy', name: 'Copy' }] : [])] })
    if (path.endsWith('/workload-tree')) return json({ folders: [], placements: [] })
    if (path.endsWith('/access')) return json({ access: { workspace_id: 'ws-prepared', access_mode: 'organization', current_role: 'owner', members: [], groups: [] } })
    if (path.endsWith('/members') || path.endsWith('/groups')) return json({ members: [], groups: [] })
    if (path.endsWith('/launch-profiles')) return json({ profiles: [] })
    if (path.includes(`/agents/${sourceId}/interface/ui/api/state`)) return json({ ready: true, runtime: { state: 'idle' }, harness: { id: 'claude' }, thread: { id: sourceSession } })
    if (path.includes(`/agents/${sourceId}/interface/ui/v1/status`)) { sourceStatusReads++; return json({ agent_id: sourceId, interface_instance_id: failure === 'instance' && sourceStatusReads > 1 ? 'other-instance' : 'source-instance', status: 'idle' }) }
    if (path.endsWith('/api/session/export')) {
      counts.export++; const body = route.request().postDataJSON()
      const owner = body.auth === 'owner-default'
      expect(body).toEqual(owner ? { threadId: sourceSession, auth: 'owner-default' } : { threadId: sourceSession })
      return json({ schema: 'treer.agent-session/v1', harness: 'claude', runtimeSnapshot: { threadId: sourceSession, agentId: sourceId, interfaceInstance: 'source-instance', ...(owner && failure !== 'source-auth-missing' ? { auth: failure === 'source-auth-mismatch' ? 'environment' : 'owner-default' } : {}) }, files: [{ path: 'history.jsonl', content: 'synthetic' }] })
    }
    if (path.endsWith('/machines/srv-prepared/files')) {
      counts.upload++; const v = route.request().postDataJSON()
      expect(v.overwrite).toBe(false); expect(v.directory).toBe('private-stage')
      expect(v.file_name).toMatch(/^treer-session-[a-f0-9-]+\.json$/)
      if (failure === 'upload') return json({ error: { code: 'unknown', message: 'synthetic unknown' } }, 503)
      const bytes = Buffer.from(v.content_base64, 'base64')
      return json({ server_id: machine.server_id, path: `${v.directory}/${v.file_name}`, bytes_written: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') })
    }
    if (path.endsWith('/machines/srv-prepared/exec')) {
      const v = route.request().postDataJSON(); const phase = v.args[1]
      if (phase === 'inspect') counts.inspect++; else { expect(phase).toBe('prepare'); counts.prepare++ }
      expect(v.command).toBe('/opt/trusted/treer-agent'); expect(v.cwd).toBe('private-stage')
      expect(v.args.slice(0, 3)).toEqual(['session', phase, '--input'])
      expect(v.args[3]).toMatch(/^\/fixture\/root\/private-stage\/treer-session-[a-f0-9-]+\.json$/)
      const owner = v.args[11] === 'owner-default'
      expect(v.args).toHaveLength(owner ? 12 : 10)
      if (owner) expect(v.args.slice(10)).toEqual(['--auth', 'owner-default'])
      expect(JSON.stringify(v)).not.toContain('synthetic')
      session = v.args[7]
      if (phase === 'prepare' && failure === 'prepare') return json({ error: { code: 'unknown', message: 'synthetic unknown' } }, 503)
      return json({ server_id: machine.server_id, cwd: v.cwd, command: v.command, args: v.args, exit_code: 0, timed_out: false, truncated: false, stdout: JSON.stringify({ schema: 'treer.claude-prepared/v1', phase: phase === 'inspect' ? 'validated' : 'prepared', inputSha256: failure === 'receipt' ? '0'.repeat(64) : v.args[9], sourceSessionId: sourceSession, sessionId: session, cwd: v.args[5], files: owner ? 1026 : 3, bytes: 200, ...(owner ? { auth: failure === 'owner-auth' ? 'environment' : 'owner-default', ownerProject: failure === 'owner-project' ? '/other/project' : failure === 'owner-key' ? '/canonical/config/projects/wrong-key' : `${failure === 'owner-canonical' || failure === 'owner-canonical-change' ? (failure === 'owner-canonical-change' && phase === 'prepare' ? '/different/config' : '/canonical/config') : '/home/fixture/.claude'}/projects/${v.args[5].replace(/[^a-zA-Z0-9]/g, '-')}`, publicationIntent: failure === 'owner-intent' || failure === 'owner-mismatch' && phase === 'prepare' ? '/other/intent.json' : `${root}/.claude-transfer-${session}.json`, environment: failure === 'owner-env' ? { CLAUDE_CONFIG_DIR: '/wrong' } : {} } : { environment: { CLAUDE_CONFIG_DIR: `${v.args[5]}/.harness/claude` } }) }), stderr: '' })
    }
    if (path === '/api/workspaces/ws-prepared/agents' && route.request().method() === 'POST') { counts.create++; payload = route.request().postDataJSON(); created = true; return json({ ...source, agent_id: 'ag-copy', name: 'Copy' }) }
    if (path === '/api/workspaces/ws-prepared/agents/ag-copy') return json({ ...source, agent_id: 'ag-copy', interface: { instance_id: 'copy-instance' } })
    if (path.includes('/agents/ag-copy/interface/ui/api/health')) return json({ ready: true })
    if (path.includes('/agents/ag-copy/interface/ui/api/state')) return json({ ready: true, runtime: { state: 'idle' }, harness: { id: 'claude' }, thread: { id: session } })
    if (path.includes('/agents/ag-copy/interface/ui/v1/status')) return json({ agent_id: 'ag-copy', interface_instance_id: 'copy-instance', status: 'idle' })
    return json({ services: [], hosts: [], traffic: [], apps: [], profiles: [], events: [], members: [], groups: [] })
  })
  return { counts, payload: () => payload }
}
async function open(page: Page, owner = false) {
  await page.goto('/orgs/org-prepared/workspaces/ws-prepared')
  await page.getByLabel('Email', { exact: true }).fill('owner@example.test')
  await page.getByLabel('Password', { exact: true }).fill('synthetic-password')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await page.getByRole('button', { name: 'Add agent', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('combobox', { name: 'Launch', exact: true }).click()
  await page.getByRole('option', { name: 'Prepared Claude target', exact: true }).click()
  await dialog.getByLabel('Working directory', { exact: true }).fill('new-copy')
  await dialog.getByLabel('Trusted launcher entry (absolute path)').fill('/opt/trusted/treer-agent')
  if (owner) {
    await dialog.getByRole('combobox', { name: 'Target authentication', exact: true }).click()
    await page.getByRole('option', { name: 'Owner default Claude configuration', exact: true }).click()
  }
  await dialog.getByRole('combobox', { name: 'Source Agent', exact: true }).click()
  await page.getByRole('option', { name: /Source Claude/ }).click()
  await dialog.getByLabel('Private staging directory (relative to machine root)').fill('private-stage')
  await dialog.getByRole('checkbox', { name: /I prepared this existing staging/ }).check()
  await dialog.getByRole('button', { name: 'Snapshot, upload and inspect once', exact: true }).click()
  return dialog
}
for (const width of [1280, 390]) test(`authenticated synthetic copy to normal Create and observed ready ${width}`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 900 })
  const fx = await fixture(page); const dialog = await open(page)
  await expect(dialog.getByText('Native validation passed. Review this plan before preparing.', { exact: true })).toBeVisible()
  await dialog.getByRole('checkbox', { name: /Prepare this new target/ }).check()
  await dialog.getByRole('button', { name: 'Prepare target once', exact: true }).click()
  await expect(dialog.getByText(/Target prepared. Confirm the normal Create/)).toBeVisible()
  await dialog.getByRole('checkbox', { name: /I confirm this target/ }).check()
  await expect(page.locator('html')).toHaveJSProperty('scrollWidth', width)
  await page.screenshot({ path: info.outputPath(`copy-${width}.png`) })
  await dialog.getByRole('button', { name: 'Create agent', exact: true }).click()
  await expect(page.getByText(/New session is ready\./)).toBeVisible()
  expect(fx.counts).toEqual({ export: 1, upload: 1, inspect: 1, prepare: 1, create: 1 })
  expect(fx.payload().args.slice(0, 4)).toEqual(['/opt/trusted/treer-agent', 'session', 'run', '--directory'])
})
for (const failure of ['upload', 'prepare', 'receipt', 'instance'] as const) test(`ambiguous ${failure} locks flow without retry`, async ({ page }) => {
  const fx = await fixture(page, failure); const dialog = await open(page)
  if (failure === 'prepare') {
    await dialog.getByRole('checkbox', { name: /Prepare this new target/ }).check()
    await dialog.getByRole('button', { name: 'Prepare target once', exact: true }).click()
  }
  await expect(dialog.getByRole('alert')).toContainText('Flow stopped.')
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click()
  await page.getByRole('button', { name: 'Add agent', exact: true }).click()
  await page.getByRole('combobox', { name: 'Launch', exact: true }).click()
  await page.getByRole('option', { name: 'Prepared Claude target', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Snapshot, upload and inspect once' })).toBeDisabled()
  expect(fx.counts.create).toBe(0); expect(fx.counts.upload).toBe(failure === 'instance' ? 0 : 1); expect(fx.counts.prepare).toBe(failure === 'prepare' ? 1 : 0)
})

test('closing during export prevents upload and cannot silently resume', async ({ page }) => {
  const fx = await fixture(page)
  let release: (() => Promise<void>) | undefined
  await page.route('**/interface/ui/api/session/export', route => { release = () => route.fulfill({ json: { schema: 'treer.agent-session/v1', harness: 'claude', runtimeSnapshot: { threadId: sourceSession, agentId: sourceId, interfaceInstance: 'source-instance' }, files: [] } }) })
  const dialog = await open(page)
  await expect.poll(() => !!release).toBe(true)
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click()
  await release!()
  await page.getByRole('button', { name: 'Add agent', exact: true }).click()
  await page.getByRole('combobox', { name: 'Launch', exact: true }).click()
  await page.getByRole('option', { name: 'Prepared Claude target', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('Flow stopped.')
  expect(fx.counts.upload).toBe(0); expect(fx.counts.prepare).toBe(0)
})

test('Chinese dark mobile copy controls are labelled and Escape returns focus', async ({ page }, info) => {
  await page.setViewportSize({ width: 390, height: 900 }); await fixture(page)
  await page.addInitScript(() => { localStorage.setItem('treer-lang', 'zh'); localStorage.setItem('treer-theme', 'dark') })
  await page.goto('/orgs/org-prepared/workspaces/ws-prepared')
  await page.locator('#email').fill('owner@example.test'); await page.locator('#password').fill('synthetic-password')
  await page.getByRole('button', { name: '登录', exact: true }).click()
  const add = page.getByRole('button', { name: '添加 Agent', exact: true }); await add.click()
  await page.getByRole('combobox', { name: '启动', exact: true }).click()
  await page.getByRole('option', { name: '已准备的 Claude 目标', exact: true }).click()
  await expect(page.getByRole('combobox', { name: '源 Agent', exact: true })).toBeVisible()
  await page.getByRole('combobox', { name: '目标认证方式', exact: true }).click()
  await page.getByRole('option', { name: 'Owner 默认 Claude 配置', exact: true }).click()
  await expect(page.getByText(/使用目标 owner 已有的 Claude 登录/)).toBeVisible()
  const ack = page.getByRole('checkbox', { name: /我已将现有暂存目录/ })
  await ack.focus(); await page.keyboard.press('Space'); await expect(ack).toBeChecked()
  await expect(page.locator('html')).toHaveJSProperty('scrollWidth', 390)
  await page.screenshot({ path: info.outputPath('copy-390-zh-dark.png') })
  await page.keyboard.press('Escape'); await expect(page.getByRole('dialog')).not.toBeVisible(); await expect(add).toBeFocused()
})

test('oversized declared export is refused before upload', async ({ page }) => {
  const fx = await fixture(page)
  await page.route('**/interface/ui/api/session/export', route => route.fulfill({ contentType: 'application/json', headers: { 'content-length': String(16 * 1024 * 1024 + 1) }, body: '{}' }))
  const dialog = await open(page)
  await expect(dialog.getByRole('alert')).toContainText('Flow stopped.')
  expect(fx.counts.upload).toBe(0); expect(fx.counts.inspect).toBe(0); expect(fx.counts.prepare).toBe(0); expect(fx.counts.create).toBe(0)
})

for (const width of [1280, 390]) test(`owner-default target auth binds inspect prepare and normal Create ${width}`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 900 })
  const fx = await fixture(page); const dialog = await open(page, true)
  await expect(dialog.getByText('Native validation passed. Review this plan before preparing.', { exact: true })).toBeVisible()
  await expect(dialog.getByRole('combobox', { name: 'Target authentication' })).toBeDisabled()
  await expect(dialog.getByText(/Uses the target owner's existing Claude login/)).toBeVisible()
  await expect(dialog.getByRole('button', { name: 'Prepare target once' })).toBeDisabled()
  await dialog.getByRole('checkbox', { name: /Prepare this new target and its exclusive owner project/ }).check()
  await dialog.getByRole('button', { name: 'Prepare target once' }).click()
  await expect(dialog.getByText(/Target prepared. Confirm the normal Create/)).toBeVisible()
  await expect(dialog.getByRole('button', { name: 'Create agent', exact: true })).toBeDisabled()
  await dialog.getByRole('checkbox', { name: /I confirm this target was prepared.*owner-default/ }).check()
  await expect(page.locator('html')).toHaveJSProperty('scrollWidth', width)
  await page.screenshot({ path: info.outputPath(`owner-default-${width}.png`) })
  await dialog.getByRole('button', { name: 'Create agent', exact: true }).click()
  await expect(page.getByText(/New session is ready\./)).toBeVisible()
  expect(fx.payload().args.slice(-2)).toEqual(['--auth', 'owner-default'])
  expect(fx.counts).toEqual({ export: 1, upload: 1, inspect: 1, prepare: 1, create: 1 })
})
for (const failure of ['owner-env', 'owner-intent', 'owner-mismatch', 'owner-auth', 'owner-project', 'prepare'] as const) test(`owner-default refuses ${failure} receipt or unknown without retry`, async ({ page }) => {
  const fx = await fixture(page, failure); const dialog = await open(page, true)
  if (failure === 'owner-mismatch' || failure === 'prepare') {
    await dialog.getByRole('checkbox', { name: /Prepare this new target and its exclusive owner project/ }).check()
    await dialog.getByRole('button', { name: 'Prepare target once' }).click()
  }
  await expect(dialog.getByRole('alert')).toContainText('Flow stopped.')
  await expect(dialog.getByRole('button', { name: 'Snapshot, upload and inspect once' })).toBeDisabled()
  expect(fx.counts.create).toBe(0)
  expect(fx.counts.prepare).toBe(failure === 'owner-mismatch' || failure === 'prepare' ? 1 : 0)
})

test('owner-default mode cannot reuse environment Create acknowledgement', () => {
  const v = { entry: '/opt/treer-agent', directory: `${root}/new-copy`, sessionId: sourceSession, acknowledgedFor: '' }
  v.acknowledgedFor = preparedClaudeBinding(machine.server_id, root, 'new-copy', v)
  expect(preparedClaudeArgs(machine.server_id, root, 'new-copy', v)?.slice(-2)).toEqual(['--auth', 'environment'])
  const changed = { ...v, auth: 'owner-default' as const }
  expect(preparedClaudeArgs(machine.server_id, root, 'new-copy', changed)).toBeNull()
  changed.acknowledgedFor = preparedClaudeBinding(machine.server_id, root, 'new-copy', changed)
  expect(preparedClaudeArgs(machine.server_id, root, 'new-copy', changed)?.slice(-2)).toEqual(['--auth', 'owner-default'])
})

for (const failure of ['source-auth-missing', 'source-auth-mismatch'] as const) test(`owner-default source export ${failure} refuses upload`, async ({ page }) => {
  const fx = await fixture(page, failure); const dialog = await open(page, true)
  await expect(dialog.getByRole('alert')).toContainText('Flow stopped.')
  expect(fx.counts).toEqual({ export: 1, upload: 0, inspect: 0, prepare: 0, create: 0 })
})

for (const mode of ['owner-canonical', 'owner-key', 'owner-canonical-change'] as const) test(`canonical owner project receipt helper ${mode}`, async ({ page }) => {
  const fx = await fixture(page, mode); const dialog = await open(page, true)
  if (mode !== 'owner-key') {
    await expect(dialog.getByText('Native validation passed. Review this plan before preparing.', { exact: true })).toBeVisible()
    await dialog.getByRole('checkbox', { name: /Prepare this new target and its exclusive owner project/ }).check()
    await dialog.getByRole('button', { name: 'Prepare target once' }).click()
  }
  if (mode === 'owner-canonical') {
    await expect(dialog.getByText(/Target prepared. Confirm the normal Create/)).toBeVisible()
    await expect(dialog.getByText(/Owner project: \/canonical\/config\/projects/)).toBeVisible()
  } else await expect(dialog.getByRole('alert')).toContainText('Flow stopped.')
  expect(fx.counts.prepare).toBe(mode === 'owner-key' ? 0 : 1)
  expect(fx.counts.create).toBe(0)
})
