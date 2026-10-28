import { expect, test, type Page } from "@playwright/test"
import { fixture, gotoWorkspace } from "./fixtures/workspace-tabs"

// `probeOk` answers the sidebar's first root request (its tree-support probe) normally, so later requests show `mode`.
async function treeFixture(page: Page, mode = "ok", probeOk = false) {
  await fixture(page)
  const requests: string[] = [], sends: any[] = []
  let messages: any[] = []
  await page.route("**/apps/app-archive/ui/api/tree?**", async route => {
    const path = new URL(route.request().url()).searchParams.get("path") ?? ""
    requests.push(path)
    if (probeOk && requests.length === 1) return route.fulfill({ json: { schema_version: 1, path: [], nodes: [] } })
    if (mode === "denied") return route.fulfill({ status: 403, json: {} })
    if (mode === "unavailable") return route.fulfill({ status: 503, json: {} })
    if (mode === "invalid") return route.fulfill({ json: { schema_version: 1, path: [], nodes: [{ key: "../escape", name: "escape", kind: "app", capabilities: ["tree.read"] }] } })
    const node = (key: string, kind: string, name: string) => ({ key, name, kind, capabilities: kind === "app" ? ["tree.read"] : ["message.send", "message.read"] })
    const nodes = mode === "empty" ? [] : path === "" ? [node("team", "app", "Team")] : path === "team" ? [node("tools", "app", "Tools")] : path === "team/tools" ? [node("agent1", "agent", "Echo")] : []
    return route.fulfill({ json: { schema_version: 1, path: path ? path.split("/") : [], nodes } })
  })
  await page.route("**/map-workspace/messages**", async route => {
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON(); sends.push(body)
      const human = { id: "map-user", name: "You" }, agent = { id: "tab-7", name: "Archive", app_endpoint: "team/tools/agent1" }
      const request = { message_id: "msg-human", sender: human, recipients: [agent], body: body.body, created_at: "2026-10-22T00:00:00Z" }
      messages = [{ message_id: "msg-reply", sender: agent, recipients: [human], body: `Echo: ${body.body}`, created_at: "2026-10-22T00:00:01Z" }, request]
      if (mode === "ambiguous" && sends.length === 1) return route.fulfill({ status: 503, json: {} })
      return route.fulfill({ json: { message: request, delivery_ids: ["delivery"], idempotent_replay: false } })
    }
    const query = new URL(route.request().url()).searchParams
    expect(query.get("app_owner_id")).toBe("tab-7"); expect(query.get("app_endpoint")).toBe("team/tools/agent1")
    return route.fulfill({ json: { messages } })
  })
  return { requests, sends }
}

for (const mobile of [false, true]) for (const theme of ["light", "dark"]) test(`App-owned nested tree conversation ${theme} ${mobile ? "390" : "desktop"}`, async ({ page }, info) => {
  await page.setViewportSize({ width: mobile ? 390 : 1280, height: 900 })
  await page.addInitScript(theme => localStorage.setItem("treer-theme", theme), theme)
  const backend = await treeFixture(page)
  await gotoWorkspace(page)
  if (mobile) await page.getByRole("button", { name: "Browse tree", exact: true }).click()
  await page.getByRole("button", { name: "App contents", exact: true }).click()
  await expect(page.getByRole("link", { name: "Team App-managed App", exact: true })).toBeVisible()
  await page.locator('[data-app-subtree="team"]').getByRole("button", { name: "App contents", exact: true }).click()
  await page.locator('[data-app-subtree="team/tools"]').getByRole("button", { name: "App contents", exact: true }).click()
  const leaf = page.getByRole("link", { name: "Echo App-managed Agent", exact: true })
  await expect(leaf).toHaveAttribute("href", /app_node=team%2Ftools%2Fagent1$/)
  await leaf.focus()
  await leaf.press("ArrowUp")
  await expect(page.getByRole("link", { name: "Tools App-managed App", exact: true })).toBeFocused()
  await page.keyboard.press("ArrowDown")
  await expect(leaf).toBeFocused()
  await leaf.press("Enter")
  await expect(page.getByRole("heading", { name: "Echo", exact: true })).toBeVisible()
  await page.getByLabel("Message", { exact: true }).fill("nested roundtrip")
  await page.getByRole("button", { name: "Send message", exact: true }).click()
  await expect(page.getByText("Echo: nested roundtrip", { exact: true })).toBeVisible()
  await expect(page.locator("[data-app-message-sender]").filter({ hasText: "Archive / team/tools/agent1" })).toHaveCount(1)
  expect(backend.sends[0].recipients).toEqual(["tab-7/team/tools/agent1"])
  expect(backend.sends[0].idempotency_key).toBeTruthy()
  await page.getByLabel("Message", { exact: true }).fill("second contextual roundtrip")
  await page.getByRole("button", { name: "Send message", exact: true }).click()
  await expect(page.getByText("Echo: second contextual roundtrip", { exact: true })).toBeVisible()
  expect(backend.sends[1].context_ids).toEqual(["msg-reply"])
  expect(backend.sends[1].idempotency_key).not.toBe(backend.sends[0].idempotency_key)
  await page.reload()
  await expect(page.getByText("Echo: second contextual roundtrip", { exact: true })).toBeVisible()
  await expect(page.locator("iframe,.xterm")).toHaveCount(0)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: info.outputPath(`app-tree-${theme}-${mobile ? "390" : "desktop"}.png`) })
  expect(backend.requests).toContain("team/tools")
})

test("App message retry retains complete payload after newer history", async ({ page }) => {
  const backend = await treeFixture(page, "ambiguous")
  await page.goto("/orgs/map-org/workspaces/map-workspace/workloads/tab-7?app_node=team%2Ftools%2Fagent1")
  await page.getByLabel("Message", { exact: true }).fill("uncertain send")
  await page.getByRole("button", { name: "Send message", exact: true }).click()
  await expect(page.getByRole("alert")).toContainText("delivery may be uncertain")
  await page.getByRole("button", { name: "Refresh messages", exact: true }).click()
  await expect(page.getByText("Echo: uncertain send", { exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Send message", exact: true }).click()
  await expect.poll(() => backend.sends.length).toBe(2)
  expect(backend.sends[1]).toEqual(backend.sends[0])
  expect(backend.sends[0].context_ids).toEqual([])
  await expect(page.getByLabel("Message", { exact: true })).toHaveValue("")
})

for (const [mode, message] of [["denied", "Access to this App tree was denied."], ["unavailable", "App tree is unavailable or returned an invalid response."]]) test(`App node deep link ${mode} is explicit`, async ({ page }) => {
  await treeFixture(page, mode)
  await page.goto("/orgs/map-org/workspaces/map-workspace/workloads/tab-7?app_node=team%2Ftools%2Fagent1")
  await expect(page.getByRole("alert")).toContainText(message)
  await expect(page.getByRole("button", { name: "Send message", exact: true })).toHaveCount(0)
})

for (const [mode, message] of [["empty", "This App subtree is empty."], ["denied", "Access to this App tree was denied."], ["unavailable", "App tree is unavailable or returned an invalid response."], ["invalid", "App tree is unavailable or returned an invalid response."]]) test(`App tree ${mode} is explicit`, async ({ page }) => {
  await treeFixture(page, mode, true)
  await gotoWorkspace(page)
  await page.getByRole("button", { name: "App contents", exact: true }).click()
  await expect(page.getByText(message, { exact: true })).toBeVisible()
  if (mode !== "empty") await expect(page.getByText("This App subtree is empty.", { exact: true })).toHaveCount(0)
})

for (const [kind, answer] of [["SPA fallback", { contentType: "text/html", body: "<!doctype html><title>App</title>" }], ["404", { status: 404, json: { error: "not_found" } }], ["501", { status: 501, body: "" }]] as const) test(`App without a tree (${kind}) is a plain leaf`, async ({ page }) => {
  await fixture(page)
  const requests: string[] = []
  await page.route("**/apps/app-archive/ui/api/tree?**", route => { requests.push(route.request().url()); return route.fulfill(answer) })
  await gotoWorkspace(page)
  const row = page.locator('[role="treeitem"][data-agent-id="tab-7"]')
  await expect.poll(() => requests.length).toBe(1)
  await expect(row).toBeVisible()
  await expect(row).not.toHaveAttribute("aria-expanded", /.*/)
  await expect(page.getByRole("button", { name: "App contents", exact: true })).toHaveCount(0)
  const link = row.locator("[data-tree-primary]").first()
  await link.focus()
  await link.press("ArrowRight")
  await expect(page.getByRole("button", { name: "App contents", exact: true })).toHaveCount(0)
  await expect(row.locator('[role="group"]')).toHaveCount(0)
  await link.click()
  await expect(page).toHaveURL(/workloads\/tab-7/)
  expect(requests).toHaveLength(1)
})

test("App tree probe failures are retried, not guessed", async ({ page }) => {
  await fixture(page)
  let calls = 0
  await page.route("**/apps/app-archive/ui/api/tree?**", route => { calls++; return route.fulfill({ status: 503, json: {} }) })
  await gotoWorkspace(page)
  await expect.poll(() => calls).toBe(1)
  await expect(page.getByRole("button", { name: "App contents", exact: true })).toHaveCount(0)
})

test("Stopped Apps are not probed", async ({ page }) => {
  let calls = 0
  await fixture(page)
  await page.route("**/apps", route => route.fulfill({ json: { apps: [{ app_id: "app-archive", runtime_agent_id: "tab-7", name: "Archive", server_id: "local", status: "stopped", hostname: "archive.test", public_url: null }] } }))
  await page.route("**/apps/app-archive/ui/api/tree?**", route => { calls++; return route.fulfill({ json: { schema_version: 1, path: [], nodes: [] } }) })
  await gotoWorkspace(page)
  await expect(page.locator('[role="treeitem"][data-agent-id="tab-7"]')).toBeVisible()
  await page.waitForTimeout(300)
  expect(calls).toBe(0)
  await expect(page.getByRole("button", { name: "App contents", exact: true })).toHaveCount(0)
})
