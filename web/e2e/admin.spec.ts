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
    if (path === "/update/check" && method === "GET") {
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
    if (path === "/invitations" && method === "GET") return ok(route, { invitations: [{ token: "inv_test", created_at: "2026-10-15T00:00:00Z", url: "https://treer.example/?invite=inv_test" }] })
    if (path === "/invitations" && method === "POST") return ok(route, { url: "https://treer.example/?invite=inv_new" })
    if (path === "/invitations/inv_test" && method === "DELETE") return ok(route, {})
    return ok(route, { error: { message: "not found" } }, 404)
  })
}

test("admin panel shows control-plane updates when the sidecar is configured", async ({ page }) => {
  await mockAdmin(page, "configured")
  await page.goto("/admin")
  await expect(page.getByRole("heading", { name: "Platform overview" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Control plane" })).toBeVisible()
  await expect(page.getByText("Channel stable")).toBeVisible()
  await page.getByRole("button", { name: "Check for updates" }).click()
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
  await page.getByRole('button', { name: 'Pending invitations', exact: true }).click()
  await expect(page.getByRole('region', { name: 'Pending invitations' }).getByRole('button', { name: 'Revoke' })).toBeVisible()
})
