import { test, expect, type Page, type Route } from "@playwright/test"

// Self-contained on purpose: this file only covers the personal menu / Settings sheet (issue #19) and must not
// depend on, or conflict with, the shared mocks in app.spec.ts.
const user = { user_id: "u1", email: "user@example.com", preferred_name: "Test User" }
const organization = { organization_id: "org-1", name: "Acme", role: "owner" as const }
const workspace = { workspace_id: "ws-1", name: "Demo", organization_id: "org-1" }

function ok(route: Route, body: unknown) {
  return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) })
}

async function mockApi(page: Page) {
  let current = { ...user }
  const calls = { logout: 0, patch: [] as unknown[] }
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/events$/, () => {})
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname.replace(/^\/api/, "")
    const method = route.request().method()
    if (path === "/auth/me") return ok(route, current)
    if (path === "/auth/profile" && method === "PATCH") { const body = route.request().postDataJSON(); calls.patch.push(body); current = { ...current, ...body }; return ok(route, current) }
    if (path === "/auth/logout" && method === "POST") { calls.logout++; return ok(route, {}) }
    if (path === "/organizations") return ok(route, { organizations: [organization] })
    if (path === "/workspaces") return ok(route, { workspaces: [workspace] })
    if (path === "/workspaces/ws-1/snapshot") return ok(route, { revision: 1, workspace, servers: [], agents: [] })
    if (path === "/workspaces/ws-1/access") return ok(route, { access: { workspace_id: "ws-1", access_mode: "organization", current_role: "owner", members: [], groups: [] } })
    if (/\/(virtual-hosts|services|launch-profiles|apps)$/.test(path)) return ok(route, { hosts: [], services: [], profiles: [], apps: [] })
    if (path === "/workspaces/ws-1/workload-tree") return ok(route, { folders: [], placements: [] })
    return route.fulfill({ status: 404, contentType: "application/json", body: "{}" })
  })
  return calls
}

const noHorizontalScroll = (page: Page) => expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)

test.describe("personal menu on a phone (390px)", () => {
  test.use({ viewport: { width: 390, height: 844 }, hasTouch: true })

  test("menu stays on screen with 44px touch targets and readable text", async ({ page }, testInfo) => {
    await mockApi(page)
    await page.goto("/")
    const trigger = page.getByRole("button", { name: "User menu" })
    const box = (await trigger.boundingBox())!
    expect(box.height).toBeGreaterThanOrEqual(44)
    expect(box.y + box.height).toBeLessThanOrEqual(844)
    expect(await trigger.locator("span").last().evaluate((el) => parseFloat(getComputedStyle(el).fontSize))).toBeGreaterThanOrEqual(11)
    await trigger.tap()
    const menu = page.getByRole("menu")
    await expect(menu).toBeVisible()
    const panel = (await menu.boundingBox())!
    expect(panel.x).toBeGreaterThanOrEqual(0)
    expect(panel.x + panel.width).toBeLessThanOrEqual(390)
    expect(panel.y).toBeGreaterThanOrEqual(0)
    expect(panel.y + panel.height).toBeLessThanOrEqual(844)
    for (const name of ["Settings", "Replay product tour", "Log out"]) {
      expect((await page.getByRole("menuitem", { name }).boundingBox())!.height).toBeGreaterThanOrEqual(44)
    }
    await noHorizontalScroll(page)
    await page.screenshot({ path: testInfo.outputPath("user-menu-390.png") })
    await page.keyboard.press("Escape")
    await expect(menu).toBeHidden()
    await expect(trigger).toBeFocused()
  })

  test("Settings is a full-screen sheet: tabs, 16px inputs, no auto-opened keyboard, profile saves", async ({ page }, testInfo) => {
    const calls = await mockApi(page)
    const pageErrors: string[] = []
    page.on("pageerror", (error) => pageErrors.push(error.message))
    await page.goto("/")
    await page.getByRole("button", { name: "User menu" }).tap()
    await page.getByRole("menuitem", { name: "Settings" }).tap()
    const dialog = page.getByRole("dialog", { name: "Settings" })
    await expect(dialog).toBeVisible()
    const sheet = (await dialog.boundingBox())!
    expect([sheet.x, sheet.y, sheet.width, sheet.height]).toEqual([0, 0, 390, 844])
    // Opening Settings must not pop the on-screen keyboard over the sheet.
    expect(await page.evaluate(() => document.activeElement?.tagName)).not.toBe("INPUT")
    const tabs = page.getByRole("navigation", { name: "Settings" }).getByRole("button")
    await expect(tabs).toHaveCount(3)
    for (const tab of await tabs.all()) expect((await tab.boundingBox())!.height).toBeGreaterThanOrEqual(44)
    const close = (await dialog.getByRole("button", { name: "Close" }).boundingBox())!
    expect(close.width).toBeGreaterThanOrEqual(40)
    expect(close.height).toBeGreaterThanOrEqual(40)
    // iOS zooms the page when a focused field is under 16px.
    for (const id of ["#settings-preferred-name", "#settings-email"]) {
      const field = page.locator(id)
      expect(await field.evaluate((el) => parseFloat(getComputedStyle(el).fontSize))).toBeGreaterThanOrEqual(16)
      expect((await field.boundingBox())!.height).toBeGreaterThanOrEqual(44)
      expect((await field.boundingBox())!.width).toBeGreaterThan(300)
    }
    await noHorizontalScroll(page)
    await page.screenshot({ path: testInfo.outputPath("settings-account-390.png") })
    await page.locator("#settings-preferred-name").fill("Renamed User")
    await page.getByRole("button", { name: "Save" }).tap()
    await expect.poll(() => calls.patch.length).toBe(1)
    await tabs.filter({ hasText: "General" }).tap()
    await expect(page.getByRole("group", { name: "Theme" })).toBeVisible()
    for (const button of await page.getByRole("group", { name: "Theme" }).getByRole("button").all()) expect((await button.boundingBox())!.height).toBeGreaterThanOrEqual(44)
    await page.getByRole("button", { name: "Dark" }).tap()
    await expect(page.locator("html")).toHaveClass(/dark/)
    await page.screenshot({ path: testInfo.outputPath("settings-general-dark-390.png") })
    await tabs.filter({ hasText: "Usage & billing" }).tap()
    await noHorizontalScroll(page)
    await dialog.getByRole("button", { name: "Close" }).tap()
    await expect(dialog).toBeHidden()
    await expect(page.getByRole("button", { name: "User menu" })).toContainText("Renamed User")
    expect(pageErrors).toEqual([])
  })

  test("Log out is reachable and works from the phone menu", async ({ page }) => {
    const calls = await mockApi(page)
    await page.goto("/")
    await page.getByRole("button", { name: "User menu" }).tap()
    await page.getByRole("menuitem", { name: "Log out" }).tap()
    await expect.poll(() => calls.logout).toBe(1)
    await expect(page.getByRole("button", { name: "User menu" })).toBeHidden()
  })
})

test.describe("personal menu on desktop keeps its compact layout", () => {
  test.use({ viewport: { width: 1280, height: 900 } })

  test("compact menu and a centred Settings dialog", async ({ page }) => {
    await mockApi(page)
    await page.goto("/")
    await page.getByRole("button", { name: "User menu" }).click()
    expect((await page.getByRole("menu").boundingBox())!.width).toBe(240)
    expect((await page.getByRole("menuitem", { name: "Settings" }).boundingBox())!.height).toBeLessThan(40)
    await page.getByRole("menuitem", { name: "Settings" }).click()
    const dialog = page.getByRole("dialog", { name: "Settings" })
    const box = (await dialog.boundingBox())!
    expect(box.width).toBeLessThanOrEqual(760)
    expect(box.x).toBeGreaterThan(100)
    await expect(page.getByRole("navigation", { name: "Settings" })).toHaveCSS("flex-direction", "column")
  })
})
