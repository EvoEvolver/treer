import { test, expect } from '@playwright/test'
import { mockHub } from './fixtures/settings-hub'

// The audit label is the name Core froze at write time, not the Agent's current path (ag-1 is now "/leader").
test('Agent actors render as "agent: <stored name>" in audit and invitations', async ({ page }) => {
  await mockHub(page, { healthCapabilities: ['settings.admin.v1'], agentInvitation: true })
  await page.goto('/orgs/org-acme/settings/audit')
  await expect(page.getByText('agent: release-bot', { exact: true })).toBeVisible()
  await expect(page.getByText('Dana Owner', { exact: true })).toBeVisible()
  await expect(page.getByText('/leader', { exact: true })).toHaveCount(0)

  await page.goto('/orgs/org-acme/settings/invitations')
  await expect(page.getByText('Invited by agent: release-bot')).toBeVisible()

  await page.route('**/api/auth/config', (route) => route.fulfill({ json: { github: false, google: false, invitation_required: false, capabilities: ['workspace.audit.v1'] } }))
  await page.goto('/orgs/org-acme/workspaces/ws-main/settings/audit')
  await expect(page.getByText('by agent: release-bot', { exact: true })).toBeVisible()
  await expect(page.getByText('by /leader', { exact: true })).toHaveCount(0)
})
