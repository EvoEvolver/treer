import { test, expect, type Page, type Route } from "@playwright/test"

function ok(route: Route, body: unknown, status = 200) {
  return route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })
}

const running = {
  channel: "stable",
  services: [
    {
      name: "proxy",
      present: true,
      version: "v0.1.2",
      digest: "sha256:aaaaaaaaaaaaaaaa",
      update_available: false,
    },
    {
      name: "app",
      present: true,
      version: "v0.1.2",
      digest: "sha256:bbbbbbbbbbbbbbbb",
      update_available: false,
    },
    {
      name: "updater",
      present: true,
      version: "v0.1.2",
      digest: "sha256:cccccccccccccccc",
      update_available: false,
    },
  ],
  job: null,
}

async function mockAdmin(page: Page, mode: "configured" | "unconfigured" = "configured") {
  await page.route("**/api/admin/**", async (route: Route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api\/admin/, "")
    const method = route.request().method()
    if (path === "/me") return ok(route, { admin: true })
    if (path === "/dashboard") {
      return ok(route, { user_count: 1, organization_count: 1, machine_count: 1, agent_count: 1 })
    }
    if (path === "/update" && method === "GET") {
      if (mode === "unconfigured") {
        return ok(route, { error: { code: "updater_unconfigured", message: "this deployment does not run a control-plane updater sidecar" } }, 404)
      }
      return ok(route, running)
    }
    if (path === "/update/check" && method === "POST") {
      return ok(route, {
        ...running,
        update_available: true,
        services: running.services.map((service) =>
          service.name === "proxy" ? { ...service, update_available: true, channel_digest: "sha256:dddddddddddddddd" } : service,
        ),
      })
    }
    if (path === "/update" && method === "POST") {
      return ok(route, { ...running, job: { id: "job1", state: "running", error: null } }, 202)
    }
    if (path === "/invitations" && method === "GET") return ok(route, { invitations: [{ invitation_id: "inv_" + "a".repeat(64), token_prefix: "inv_demo", created_at: "2026-10-15T00:00:00Z", used_at: null }] })
    if (path === "/invitations" && method === "POST") return ok(route, { url: "https://treer.example/?invite=inv_new" })
    if (path === "/invitations/inv_" + "a".repeat(64) && method === "DELETE") return ok(route, {})
    return ok(route, { error: { message: "not found" } }, 404)
  })
}

test("admin panel shows control-plane updates when the sidecar is configured", async ({ page }) => {
  await mockAdmin(page, "configured")
  await page.goto("/admin")
  await expect(page.getByRole("heading", { name: "Platform overview" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Control plane" })).toBeVisible()
  await expect(page.getByText("Channel stable")).toBeVisible()
  const checking = page.waitForRequest((request) => request.url().endsWith("/api/admin/update/check"))
  await page.getByRole("button", { name: "Check for updates" }).click()
  expect((await checking).method()).toBe("POST")
  await expect(page.getByText("update available")).toBeVisible()
  await expect(page.getByRole("button", { name: "Apply update" })).toBeEnabled()
})

test("admin panel explains when control-plane updates are not configured", async ({ page }) => {
  await mockAdmin(page, "unconfigured")
  await page.goto("/admin")
  await expect(page.getByRole("heading", { name: "Control plane" })).toBeVisible()
  await expect(page.getByText("Control-plane updates are not configured on this deployment.")).toBeVisible()
  await expect(page.getByRole("button", { name: "Apply update" })).toHaveCount(0)
})

for (const theme of ["light", "dark"] as const) for (const width of [1360, 390]) {
  test(`admin shows only aggregate resource counts ${theme} ${width}`, async ({ page }, info) => {
    const requested: string[] = []
    page.on("request", (request) => { if (request.url().includes("/api/admin/")) requested.push(new URL(request.url()).pathname) })
    await mockAdmin(page)
    await page.setViewportSize({ width, height: 844 })
    await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
    await page.goto("/admin")
    await page.evaluate((value) => document.documentElement.classList.toggle('dark', value === 'dark'), theme)
    const totals = page.getByRole("group", { name: "Platform resource totals" })
    await expect(totals).toBeVisible()
    for (const label of ['Users', 'Organizations', 'Machines', 'Agents']) await expect(totals.getByText(label, { exact: true })).toBeVisible()
    await expect(totals.getByText('1', { exact: true })).toHaveCount(4)
    await expect(totals.getByRole('button')).toHaveCount(0)
    for (const name of ['View organizations', 'View activity', 'Reset password', 'Sign out everywhere', 'Details']) await expect(page.getByRole('button', { name, exact: true })).toHaveCount(0)
    const beforeRefresh = requested.filter((path) => path === '/api/admin/dashboard').length
    await page.getByRole('button', { name: 'Refresh', exact: true }).click()
    await expect.poll(() => requested.filter((path) => path === '/api/admin/dashboard').length).toBeGreaterThan(beforeRefresh)
    expect(requested.some((path) => /^\/api\/admin\/(users|machines|agents|organizations|activity)(?:\/|$)/.test(path))).toBe(false)
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: info.outputPath(`admin-private-${theme}-${width}.png`) })
  })
}

test('admin keeps platform invitation management separate from private inventories', async ({ page }) => {
  await mockAdmin(page)
  await page.goto('/admin')
  await page.getByRole('button', { name: 'Create invitation', exact: true }).click()
  await expect(page.getByRole('dialog')).toBeVisible()
  await expect(page.getByRole('textbox')).toHaveValue('https://treer.example/?invite=inv_new')
  await page.getByRole('button', { name: 'Close', exact: true }).first().click()
  await page.getByRole('button', { name: 'Registration invitations', exact: true }).click()
  await expect(page.getByRole('region', { name: 'Registration invitations' }).getByRole('button', { name: 'Revoke' })).toBeVisible()
})


test('invitation summaries show prefixes and used state, revoke by non-bearer handle', async ({ page }, info) => {
  await mockAdmin(page)
  const id = 'inv_' + 'b'.repeat(64)
  const usedId = 'inv_' + 'c'.repeat(64)
  let pending = true
  const revoked: string[] = []
  await page.route('**/api/admin/invitations**', async (route) => {
    if (route.request().method() === 'DELETE') { revoked.push(new URL(route.request().url()).pathname); pending = false; return ok(route, {}) }
    return ok(route, { invitations: [
      ...(pending ? [{ invitation_id: id, token_prefix: 'inv_demo', created_at: '2026-10-15T00:00:00Z', used_at: null }] : []),
      { invitation_id: usedId, token_prefix: 'inv_used', created_at: '2026-10-14T00:00:00Z', used_at: '2026-10-15T01:00:00Z' },
      { invitation_id: 'inv_' + 'd'.repeat(64), token_prefix: '[redacted]', created_at: '2026-10-13T00:00:00Z', used_at: null },
    ] })
  })
  await page.goto('/admin')
  await page.getByRole('button', { name: 'Registration invitations', exact: true }).click()
  const section = page.getByRole('region', { name: 'Registration invitations' })
  await expect(section.getByText('inv_demo…', { exact: true })).toBeVisible()
  await expect(section.getByText('Used', { exact: true })).toBeVisible()
  await expect(section.getByText('[redacted]', { exact: true })).toBeVisible()
  await expect(section.getByRole('alert')).toHaveCount(0)
  await expect(section.getByRole('button', { name: 'Copy', exact: true })).toHaveCount(0)
  await expect(section.getByRole('button', { name: 'Revoke' }).nth(1)).toBeDisabled()
  await page.screenshot({ path: info.outputPath('admin-invitation-summaries.png'), fullPage: true })
  await section.getByRole('button', { name: 'Revoke' }).first().click()
  await expect(section.getByText('inv_demo…', { exact: true })).toHaveCount(0)
  expect(revoked).toEqual([`/api/admin/invitations/${id}`])
})

test('legacy invitation list is refused without rendering bearer links or offering copy/revoke', async ({ page }) => {
  await mockAdmin(page)
  await page.route('**/api/admin/invitations', (route) => ok(route, { invitations: [{ token: 'inv_sensitive_secret', created_at: '2026-10-15T00:00:00Z', url: 'https://treer.example/?invite=inv_sensitive_secret' }] }))
  await page.goto('/admin')
  await page.getByRole('button', { name: 'Registration invitations', exact: true }).click()
  const section = page.getByRole('region', { name: 'Registration invitations' })
  await expect(section.getByRole('alert')).toContainText('does not support private invitation summaries')
  await expect(page.getByText(/inv_sensitive_secret/)).toHaveCount(0)
  await expect(section.getByRole('button')).toHaveCount(0)
})
