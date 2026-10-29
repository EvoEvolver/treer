import { test, expect } from '@playwright/test'
import { mockHub, noHorizontalScroll } from './fixtures/settings-hub'

const grantsProxy = ['settings.admin.v1', 'org.agent-grants.v1']

// Owners/admins decide which Agents may administer the organization; Core also requires the Agent's workspace Policy.
test('owners grant, re-save and remove Agent organization permissions', async ({ page }) => {
  const backend = await mockHub(page, { healthCapabilities: grantsProxy })
  await page.goto('/orgs/org-acme/settings/members')
  await page.getByRole('navigation', { name: 'Settings' }).getByRole('link', { name: 'Agent permissions' }).click()
  await expect(page).toHaveURL(/\/orgs\/org-acme\/settings\/agent-permissions$/)
  await expect(page.getByRole('heading', { name: 'Agent permissions', exact: true })).toBeVisible()
  await expect(page.getByRole('note').filter({ hasText: 'names the Agent by id' })).toContainText('policy.provider.manage is never grantable')
  const table = page.getByRole('table', { name: 'Agent permissions' })
  const trainer = table.getByRole('row', { name: /trainer/ })
  await expect(trainer).toContainText('ag-2')
  await expect(trainer).toContainText('Invitations')
  await expect(trainer).toContainText('Dana Owner')
  await expect(trainer).toContainText('Active')
  const old = table.getByRole('row', { name: /ag-old/ })
  await expect(old).toContainText('Inactive')
  await expect(old).toContainText('Creator lost owner/admin role; re-save to reactivate.')
  await expect(old.getByRole('button', { name: 'Edit permissions for ag-old' })).toHaveText('Re-save')

  // Pick an Agent of the chosen workspace and two permissions.
  await page.getByRole('button', { name: 'Grant permissions' }).click()
  const dialog = page.getByRole('dialog', { name: 'Grant an Agent organization permissions' })
  await expect(dialog.getByRole('button', { name: 'Save' })).toBeDisabled()
  await dialog.getByRole('combobox', { name: 'Workspace' }).click()
  await page.getByRole('option', { name: 'Main' }).click()
  await dialog.getByRole('combobox', { name: 'Pick an Agent' }).fill('lead')
  await page.getByRole('option', { name: /leader/ }).click()
  await expect(dialog.getByRole('textbox', { name: 'Agent ID' })).toHaveValue('ag-1')
  await dialog.getByRole('checkbox', { name: /^Audit log/ }).check()
  await dialog.getByRole('checkbox', { name: /^Members/ }).check()
  await dialog.getByRole('button', { name: 'Save' }).click()
  await expect(dialog).toBeHidden()
  expect(backend.calls.filter((call) => call.method === 'PUT')).toEqual([{ method: 'PUT', path: '/organizations/org-acme/agent-grants/ag-1', body: { workspace_id: 'ws-main', actions: ['org.member.manage', 'audit.read'] } }])
  await expect(table.getByRole('row', { name: /leader|ag-1/ })).toContainText('Audit log')

  // An id can be pasted for an Agent the picker does not list.
  await page.getByRole('button', { name: 'Grant permissions' }).click()
  await dialog.getByRole('combobox', { name: 'Workspace' }).click()
  await page.getByRole('option', { name: 'Security review' }).click()
  await dialog.getByRole('textbox', { name: 'Agent ID' }).fill('ag-pasted')
  await dialog.getByRole('checkbox', { name: /^Groups/ }).check()
  await dialog.getByRole('button', { name: 'Save' }).click()
  await expect.poll(() => backend.calls.at(-1)).toEqual({ method: 'PUT', path: '/organizations/org-acme/agent-grants/ag-pasted', body: { workspace_id: 'ws-sec', actions: ['org.group.manage'] } })

  // Re-saving an inactive grant keeps its Agent and makes it active again.
  await old.getByRole('button', { name: 'Edit permissions for ag-old' }).click()
  const edit = page.getByRole('dialog', { name: 'Edit permissions for ag-old' })
  await expect(edit.getByRole('textbox', { name: 'Agent ID' })).toBeDisabled()
  await expect(edit.getByRole('checkbox', { name: /^Audit log/ })).toBeChecked()
  await edit.getByRole('button', { name: 'Save' }).click()
  await expect.poll(() => backend.calls.at(-1)).toEqual({ method: 'PUT', path: '/organizations/org-acme/agent-grants/ag-old', body: { workspace_id: 'ws-sec', actions: ['audit.read'] } })
  await expect(table.getByRole('row', { name: /ag-old/ })).toContainText('Active')

  await trainer.getByRole('button', { name: 'Remove permissions for trainer' }).click()
  await page.getByRole('dialog', { name: 'Remove permissions for trainer?' }).getByRole('button', { name: 'Remove' }).click()
  await expect.poll(() => backend.calls.at(-1)).toEqual({ method: 'DELETE', path: '/organizations/org-acme/agent-grants/ag-2', body: null })
  await expect(table.getByRole('row', { name: /trainer/ })).toHaveCount(0)

  const api = page.locator('[data-api-equivalent]')
  await api.locator('summary').click()
  await expect(api).toContainText('PUT/api/organizations/org-acme/agent-grants/{agent_id}')
  await expect(api).toContainText('DELETE/api/organizations/org-acme/agent-grants/{agent_id}')

  await page.setViewportSize({ width: 390, height: 844 })
  await page.reload()
  await expect(page.getByRole('table', { name: 'Agent permissions' })).toBeVisible()
  expect(await noHorizontalScroll(page)).toBe(true)
})

test('Agent permissions are hidden for members and on a Proxy without org.agent-grants.v1', async ({ page }) => {
  await mockHub(page, { orgRole: 'member', healthCapabilities: grantsProxy })
  await page.goto('/orgs/org-acme/settings/general')
  const nav = page.getByRole('navigation', { name: 'Settings' })
  await expect(nav.getByRole('link', { name: 'Groups' })).toBeVisible()
  await expect(nav.getByRole('link', { name: 'Agent permissions' })).toHaveCount(0)
  await page.goto('/orgs/org-acme/settings/agent-permissions')
  await expect(page.getByRole('note').filter({ hasText: 'Only owners and admins can see and change Agent permissions.' })).toBeVisible()
  await expect(page.getByRole('table', { name: 'Agent permissions' })).toHaveCount(0)

  await page.unrouteAll({ behavior: 'ignoreErrors' })
  await mockHub(page)
  await page.goto('/orgs/org-acme/settings/general')
  await expect(nav.getByRole('link', { name: 'Groups' })).toBeVisible()
  await expect(nav.getByRole('link', { name: 'Agent permissions' })).toHaveCount(0)
  await page.goto('/orgs/org-acme/settings/agent-permissions')
  await expect(page.getByRole('status').filter({ hasText: 'This Proxy does not support organization Agent permissions yet.' })).toBeVisible()
  await page.goto('/orgs/org-acme/settings/members')
  await expect(page.getByRole('heading', { name: 'Members', exact: true })).toBeVisible()
  await expect(page.getByText(/Agents can invite members only/)).toHaveCount(0)
})

// The audit label is the name Core froze at write time, not the Agent's current path (ag-1 is now "/leader").
test('Agent actors render as "agent: <stored name>" in audit and invitations', async ({ page }) => {
  await mockHub(page, { healthCapabilities: grantsProxy, agentInvitation: true })
  await page.goto('/orgs/org-acme/settings/audit')
  await expect(page.getByText('agent: release-bot', { exact: true })).toBeVisible()
  await expect(page.getByText('Dana Owner', { exact: true })).toBeVisible()
  await expect(page.getByText('/leader', { exact: true })).toHaveCount(0)

  await page.goto('/orgs/org-acme/settings/invitations')
  await expect(page.getByText('Invited by agent: release-bot')).toBeVisible()
  await expect(page.getByRole('note').filter({ hasText: 'Agents can invite members only' })).toContainText('cannot invite owners or admins')
  await page.goto('/orgs/org-acme/settings/members')
  await expect(page.getByRole('note').filter({ hasText: 'Agents can invite members only' })).toBeVisible()

  await page.route('**/api/auth/config', (route) => route.fulfill({ json: { github: false, google: false, invitation_required: false, capabilities: ['workspace.audit.v1'] } }))
  await page.goto('/orgs/org-acme/workspaces/ws-main/settings/audit')
  await expect(page.getByText('by agent: release-bot', { exact: true })).toBeVisible()
  await expect(page.getByText('by /leader', { exact: true })).toHaveCount(0)
})
