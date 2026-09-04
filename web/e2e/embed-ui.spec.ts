import { test, expect, type Page, type Route } from "@playwright/test"

const NOW = new Date().toISOString()

const user = {
  user_id: "u1",
  email: "user@example.com",
  preferred_name: "Test User",
}

const organization = {
  organization_id: "org-1",
  name: "Acme",
  role: "owner" as const,
}

const workspace = {
  workspace_id: "ws-1",
  name: "Demo",
  organization_id: "org-1",
}

const build = { version: "0.1.2", git_commit: "abcdef1234567890" }

const machine = {
  server_id: "srv-a",
  name: "workstation",
  hostname: "workstation.lan",
  root: "/Users/test/worker",
  controller_build: build,
  host_build: build,
  status: "online",
}

// Two agents on the same machine:
// - ag-ui: exposes a UI through its Agent Interface descriptor.
// - ag-tty: a plain terminal agent without an Interface UI.
const agentWithUi = {
  agent_id: "ag-ui",
  server_id: "srv-a",
  name: "dashboard",
  kind: "command",
  status: "running",
  interface: {
    protocol: "treer.agent-interface/v1",
    instance_id: "interface-ui-1",
    port: 8080,
    capabilities: [],
    ui_path: "/",
    registered_at: NOW,
  },
}

const agentPlain = {
  agent_id: "ag-tty",
  server_id: "srv-a",
  name: "plain-tty",
  kind: "command",
  status: "running",
}

const agentAcp = {
  ...agentWithUi,
  agent_id: "ag-acp",
  name: "grok-thread",
  kind: "acp",
}

const service = {
  service_id: "svc-1",
  name: "dashboard-app",
  server_id: "srv-a",
  target_host: "127.0.0.1",
  target_port: 8080,
  protocol: "http" as const,
  updated_at: NOW,
  updated_by: "user@example.com",
}

const snapshot = {
  revision: 1,
  workspace,
  servers: [machine],
  agents: [agentWithUi, agentPlain, agentAcp],
}

function ok(route: Route, body: unknown) {
  return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) })
}

async function mockApi(page: Page) {
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/events$/, () => {})
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/agents\/[^/]+\/terminal(?:\?.*)?$/, () => {})
  await page.route("**/api/**", async (route: Route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api/, "")

    if (path === "/auth/me") return ok(route, user)
    if (path === "/organizations") return ok(route, { organizations: [organization] })
    if (path === "/organizations/org-1/members") return ok(route, { members: [{ user_id: user.user_id, email: user.email, preferred_name: user.preferred_name, role: "owner" }] })
    if (path === "/organizations/org-1/audit-events") return ok(route, { events: [] })
    if (path === "/workspaces") return ok(route, { workspaces: url.searchParams.get("organization_id") === "org-1" ? [workspace] : [] })
    if (path === "/workspaces/ws-1/snapshot") return ok(route, snapshot)
    if (path === "/workspaces/ws-1/virtual-hosts") return ok(route, { hosts: [] })
    if (path === "/workspaces/ws-1/services") return ok(route, { services: [service] })
    if (path === "/workspaces/ws-1/ingresses") return ok(route, { ingresses: [] })
    if (path === "/workspaces/ws-1/traffic") return ok(route, { traffic: [] })
    if (path === "/workspaces/ws-1/launch-profiles") return ok(route, { profiles: [] })

    return route.fulfill({ status: 404, contentType: "application/json", body: "{}" })
  })
}

async function mockInterfaceUiContent(page: Page) {
  // Intercept the Interface UI iframe URL and serve a small HTML page so we can
  // verify the iframe actually loads that content (and not a blank page).
  await page.route("**/api/workspaces/*/agents/*/interface/ui/**", async (route: Route) => {
    await route.fulfill({
      status: 200,
      contentType: "text/html",
      body: `<!doctype html><html><body><h1 data-testid="agent-app">Agent dashboard for ag-ui</h1></body></html>`,
    })
  })
}

test.beforeEach(async ({ page }) => {
  await mockApi(page)
  await mockInterfaceUiContent(page)
})

test("selecting an agent with an embedded UI shows the iframe instead of the terminal pane", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("tab", { name: /Agents/ }).click()

  // Click the agent that has a UI
  await page.getByRole("button", { name: /^dashboard / }).click()

  // The terminal pane (xterm host) must NOT be rendered
  await expect(page.locator(".xterm")).toBeHidden()

  // The iframe must be rendered, pointing at the UI proxy URL
  const frame = page.locator("iframe[title='dashboard interface']")
  await expect(frame).toBeVisible()
  await expect(frame).toHaveAttribute("src", /\/api\/workspaces\/ws-1\/agents\/ag-ui\/interface\/ui\//)

  // And the iframe must have the sandbox restrictions we configured
  const sandbox = await frame.getAttribute("sandbox")
  expect(sandbox).toContain("allow-scripts")
  expect(sandbox).toContain("allow-same-origin")
  expect(sandbox).not.toContain("allow-top-navigation")

  // The framed content was actually loaded
  const innerHeading = page.frameLocator("iframe[title='dashboard interface']").getByText("Agent dashboard for ag-ui")
  await expect(innerHeading).toBeVisible()
})

test("selecting an embedded UI agent on an offline machine does not load the iframe", async ({ page }) => {
  await page.unroute("**/api/workspaces/ws-1/snapshot")
  await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, {
    ...snapshot,
    servers: [{ ...machine, status: "offline" }],
  }))
  await page.goto("/")
  await page.getByRole("tab", { name: /Agents/ }).click()
  await page.getByRole("button", { name: /^dashboard / }).click()
  await expect(page.getByText("Machine is offline")).toBeVisible()
  await expect(page.locator("iframe[title='dashboard interface']")).toBeHidden()
  await expect(page.getByText("offline").first()).toBeVisible()
  await expect(page.getByText("treer-agent-server service --workspace ws-1 restart-controller")).toBeVisible()
  await expect(page.getByRole("button", { name: "Copy restart-controller" })).toBeVisible()
  await expect(page.getByRole("button", { name: "Copy start" })).toBeVisible()
})

test("ACP thread iframe appends Treer embed chrome flags", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("tab", { name: /Agents/ }).click()
  await page.getByRole("button", { name: /^grok-thread / }).click()
  const frame = page.locator("iframe[title='grok-thread interface']")
  await expect(frame).toBeVisible()
  await expect(frame).toHaveAttribute("src", /presentation=workspace/)
  await expect(frame).toHaveAttribute("src", /explorer=1/)
  await expect(frame).toHaveAttribute("src", /shell=0/)
  await expect(frame).toHaveAttribute("src", /permissions=0/)
  await expect(frame).toHaveAttribute("src", /nav=0/)
})

test("selecting a plain terminal agent shows the terminal pane, not an iframe", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("tab", { name: /Agents/ }).click()

  // Click the plain agent without an Interface UI.
  await page.getByRole("button", { name: /^plain-tty / }).click()

  // No iframe should be rendered and the terminal pane is shown instead
  await expect(page.locator("iframe[title='plain-tty interface']")).toBeHidden()
})

test("reload button label changes when an agent has an embedded UI", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("tab", { name: /Agents/ }).click()

  // Plain agent: Reconnect terminal
  await page.getByRole("button", { name: /^plain-tty / }).click()
  await expect(page.getByRole("button", { name: "Reconnect terminal" })).toBeVisible()

  // Agent with UI: label switches to Reload interface
  await page.getByRole("button", { name: /^dashboard / }).click()
  await expect(page.getByRole("button", { name: "Reload interface" })).toBeVisible()
})

test("mobile: selecting an embedded UI agent opens a full-screen iframe", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await page.getByRole("tab", { name: /Agents/ }).click()

  await expect(page.locator("aside")).toBeVisible()
  await expect(page.locator("iframe[title='dashboard interface']")).toBeHidden()

  await page.getByRole("button", { name: /^dashboard / }).click()
  const frame = page.locator("iframe[title='dashboard interface']")
  await expect(frame).toBeVisible()
  await expect(page.getByRole("button", { name: "Close full-screen interface" })).toBeVisible()
  await expect(page.locator(".xterm")).toBeHidden()

  await page.getByRole("button", { name: "Close full-screen interface" }).click()
  await expect(frame).toBeHidden()
  await expect(page.locator("aside")).toBeVisible()
})

test("reload button triggers an iframe reload (key changes)", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("tab", { name: /Agents/ }).click()
  await page.getByRole("button", { name: /^dashboard / }).click()

  // Track how many times the iframe's URL is fetched
  let uiProxyHits = 0
  await page.route("**/api/workspaces/*/agents/*/interface/ui/**", async (route) => {
    uiProxyHits += 1
    await route.fulfill({
      status: 200,
      contentType: "text/html",
      body: "<!doctype html><html><body>iframe reload test</body></html>",
    })
  })

  const hitsBefore = uiProxyHits
  await page.getByRole("button", { name: "Reload interface" }).click()

  // The iframe should re-fetch its src
  await expect.poll(() => uiProxyHits, { timeout: 5000 }).toBeGreaterThan(hitsBefore)
})
