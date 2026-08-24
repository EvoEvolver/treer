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

const machineA = {
  server_id: "srv-a",
  name: "workstation",
  hostname: "workstation.lan",
  root: "/Users/test/worker",
  controller_build: build,
  host_build: build,
  status: "online",
}

const machineB = {
  ...machineA,
  server_id: "srv-b",
  name: "cloudbox",
  hostname: "cloudbox.example.com",
  root: "/srv/cloud",
  status: "online",
}

const agentA = {
  agent_id: "ag-1",
  server_id: "srv-a",
  name: "api-server",
  kind: "command",
  status: "running",
}

const agentB = {
  agent_id: "ag-2",
  server_id: "srv-b",
  name: "worker",
  kind: "terminal",
  status: "running",
}

const serviceA = {
  service_id: "svc-1",
  name: "api",
  server_id: "srv-a",
  target_host: "127.0.0.1",
  target_port: 3000,
  protocol: "http" as const,
  updated_at: NOW,
  updated_by: "user@example.com",
}

const virtualHost = {
  hostname: "api.demo.example.com",
  service_id: "svc-1",
  service_protocol: "http" as const,
  destination_server_id: "srv-a",
  target_host: "127.0.0.1",
  target_port: 3000,
}

const recipeProfile = {
  profile_id: "alp-recipe",
  workspace_id: "ws-1",
  name: "Codex Agent UI",
  description: "Codex app-server plus the thread UI iframe",
  cwd: "codex-agent-ui",
  command: "./scripts/treer-agent.sh",
  args: [] as string[],
  created_at: NOW,
  created_by: "u1",
  updated_at: NOW,
  updated_by: "u1",
}

const snapshot = {
  revision: 1,
  workspace,
  servers: [machineA, machineB],
  agents: [agentA, agentB],
}

const traffic = [
  { window_start: NOW, source_server_id: "srv-a", destination_server_id: "srv-b", payload_bytes: 1500, payload_frames: 12 },
  { window_start: NOW, source_server_id: "srv-b", destination_server_id: "srv-a", payload_bytes: 750, payload_frames: 9 },
]

function ok(route: Route, body: unknown) {
  return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) })
}

async function mockApi(page: Page) {
  let currentUser = { ...user }
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/events$/, () => {})
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/agents\/[^/]+\/terminal(?:\?.*)?$/, () => {})
  await page.route("**/api/**", async (route: Route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api/, "")

    if (path === "/auth/me") return ok(route, currentUser)
    if (path === "/auth/profile" && route.request().method() === "PATCH") {
      const body = route.request().postDataJSON() as { email?: string; preferred_name?: string }
      currentUser = {
        ...currentUser,
        email: body.email ?? currentUser.email,
        preferred_name: body.preferred_name ?? currentUser.preferred_name,
      }
      return ok(route, currentUser)
    }
    if (path === "/organizations") return ok(route, { organizations: [organization] })
    if (path === "/organizations/org-1/members") return ok(route, { members: [{ user_id: user.user_id, email: user.email, preferred_name: user.preferred_name, role: "owner" }] })
    if (path === "/organizations/org-1/audit-events") return ok(route, { events: [] })
    if (path === "/workspaces") return ok(route, { workspaces: url.searchParams.get("organization_id") === "org-1" ? [workspace] : [] })
    if (path === "/workspaces/ws-1/snapshot") return ok(route, snapshot)
    if (path === "/workspaces/ws-1/virtual-hosts") return ok(route, { hosts: [virtualHost] })
    if (path === "/workspaces/ws-1/services") return ok(route, { services: [serviceA] })
    if (path === "/workspaces/ws-1/ingresses") return ok(route, { ingresses: [] })
    if (path === "/workspaces/ws-1/traffic") return ok(route, { traffic })
    if (path === "/workspaces/ws-1/launch-profiles") return ok(route, { profiles: [recipeProfile] })

    return route.fulfill({ status: 404, contentType: "application/json", body: "{}" })
  })
}

test.beforeEach(async ({ page }) => {
  await mockApi(page)
})

// Helpers — machine entries in the machines list are <button>s whose accessible
// name includes the machine name, root path and build label.
const workstationRow = (page: Page) => page.getByRole("button", { name: /^workstation / })
const cloudboxRow = (page: Page) => page.getByRole("button", { name: /^cloudbox / })
const agentsTab = (page: Page) => page.getByRole("tab", { name: /Agents/ })
const machinesTab = (page: Page) => page.getByRole("tab", { name: /Machines/ })

test("opens app, shows org, workspace, machines and agents", async ({ page }) => {
  await page.goto("/")
  await expect(page.locator("aside").getByText("Acme")).toBeVisible()
  await expect(page.getByRole("combobox", { name: "Workspace" })).toHaveText("Demo")

  await machinesTab(page).click()
  await expect(workstationRow(page)).toBeVisible()
  await expect(cloudboxRow(page)).toBeVisible()

  await agentsTab(page).click()
  await expect(page.getByRole("button", { name: /^api-server / })).toBeVisible()
  await expect(page.getByRole("button", { name: /^worker / })).toBeVisible()
})

test("agent list row menu can rename and delete", async ({ page }) => {
  await page.goto("/")
  await agentsTab(page).click()
  const row = page.getByRole("button", { name: /^api-server / })
  await row.hover()
  await page.getByRole("button", { name: "Actions for api-server" }).click()
  await expect(page.getByRole("menuitem", { name: "Rename" })).toBeVisible()
  await expect(page.getByRole("menuitem", { name: "Stop" })).toBeVisible()
  await expect(page.getByRole("menuitem", { name: "Delete" })).toBeVisible()
  await page.getByRole("menuitem", { name: "Rename" }).click()
  await expect(page.getByRole("heading", { name: "Rename agent" })).toBeVisible()
  await page.getByRole("button", { name: "Cancel" }).click()
  await row.hover()
  await page.getByRole("button", { name: "Actions for api-server" }).click()
  await page.getByRole("menuitem", { name: "Delete" }).click()
  await expect(page.getByRole("heading", { name: "Delete agent" })).toBeVisible()
})

test("org dropdown contains Members and Audit entries", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("button", { name: "Organization actions" }).click()
  await expect(page.getByRole("menuitem", { name: "Create organization" })).toBeVisible()
  await expect(page.getByRole("menuitem", { name: "Members" })).toBeVisible()
  await expect(page.getByRole("menuitem", { name: "Audit" })).toBeVisible()
})

test("workspace dropdown opens Profiles and Network", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("button", { name: /Workspace/ }).click()
  await page.getByRole("menuitem", { name: "Profiles" }).click()
  await expect(page.getByRole("heading", { name: "Profiles" })).toBeVisible()

  await page.getByRole("button", { name: /Workspace|Profiles/ }).click()
  await page.getByRole("menuitem", { name: "Network" }).click()
  await expect(page.getByRole("heading", { name: "Network" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Machine services" })).toBeVisible()
})

test("service registration can target an Agent's private loopback", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("button", { name: /Workspace/ }).click()
  await page.getByRole("menuitem", { name: "Network" }).click()
  await page.getByRole("button", { name: "Add service" }).click()

  const dialog = page.getByRole("dialog", { name: "Register service" })
  const selects = dialog.getByRole("combobox")
  await selects.nth(0).click()
  await page.getByRole("option", { name: /api-server.*workstation/ }).click()

  await expect(selects.nth(1)).toBeDisabled()
  const targetHost = dialog.getByText("Target host", { exact: true }).locator("..").getByRole("textbox")
  await expect(targetHost).toBeDisabled()
  await expect(targetHost).toHaveValue("127.0.0.1")
})

test("clicking a machine opens an overview with identity, agents, services, virtual hosts, and traffic", async ({ page }) => {
  await page.goto("/")
  await machinesTab(page).click()
  await workstationRow(page).click()

  // Identity
  await expect(page.getByRole("heading", { name: "workstation" })).toBeVisible()
  await expect(page.getByText("srv-a")).toBeVisible()
  await expect(page.getByText("workstation.lan")).toBeVisible()
  await expect(page.getByTitle("/Users/test/worker")).toBeVisible()

  // Agents on this machine only (not the worker on cloudbox)
  await expect(page.getByText("Agents on this machine")).toBeVisible()
  await expect(page.getByText("ag-1")).toBeVisible()

  // Registered services on this machine
  await expect(page.getByText("Registered services")).toBeVisible()
  await expect(page.getByText(/^127\.0\.0\.1:3000/).first()).toBeVisible()

  // Virtual hosts targeting this machine
  await expect(page.getByText("Virtual hosts", { exact: true }).first()).toBeVisible()
  await expect(page.getByText("api.demo.example.com")).toBeVisible()

  // Network summary (traffic totals + peers)
  await expect(page.getByText("Network (last 24h)")).toBeVisible()
  await expect(page.getByText("Data sent")).toBeVisible()
  await expect(page.getByText("Data received")).toBeVisible()

  await page.getByRole("button", { name: "Close" }).click()
  await expect(page.getByRole("heading", { name: "workstation" })).toBeHidden()
})

test("machine overview only shows agents for the selected machine", async ({ page }) => {
  await page.goto("/")
  await machinesTab(page).click()
  await workstationRow(page).click()
  await expect(page.getByRole("heading", { name: "workstation" })).toBeVisible()

  // Only the agent on srv-a should appear in the overview agent list
  const agentsSection = page.locator("main section", { has: page.getByRole("heading", { name: "Agents on this machine" }) })
  await expect(agentsSection.getByRole("button", { name: "Terminal" })).toBeVisible()
  await expect(agentsSection.getByText(/ag-1/)).toBeVisible()
  await expect(agentsSection.getByText(/ag-2/)).toBeHidden()

  // Switch to cloudbox — only its agent should show
  await page.getByRole("button", { name: "Close" }).click()
  await cloudboxRow(page).click()
  await expect(page.getByRole("heading", { name: "cloudbox" })).toBeVisible()
  await expect(page.locator("main").getByText(/ag-2/)).toBeVisible()
  await expect(page.locator("main").getByText(/ag-1/)).toBeHidden()
})

test("from a machine overview, clicking Terminal opens the agent terminal", async ({ page }) => {
  await page.goto("/")
  await machinesTab(page).click()
  await workstationRow(page).click()
  await page.getByRole("button", { name: "Terminal" }).click()

  // Returns to the terminal main view; header shows agent name
  await expect(page.locator("header").getByText("api-server")).toBeVisible()
})

test("mobile: opening network hides sidebar, back button returns to terminal", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")

  await page.getByRole("button", { name: /Workspace/ }).click()
  await page.getByRole("menuitem", { name: "Network" }).click()
  await expect(page.getByRole("heading", { name: "Network" })).toBeVisible()

  const aside = page.locator("aside")
  await expect(aside).toBeHidden()

  await page.getByRole("button", { name: "Back" }).dispatchEvent("click")
  await expect(aside).toBeVisible()
})

test("create agent dialog can install a git recipe", async ({ page }) => {
  await page.goto("/")
  await agentsTab(page).click()
  await page.getByRole("button", { name: "New" }).click()
  await expect(page.getByRole("dialog")).toBeVisible()
  await page.getByRole("dialog").getByRole("combobox").first().click()
  await page.getByRole("option", { name: "Install recipe" }).click()
  await expect(page.getByPlaceholder("https://github.com/example/recipe.git")).toBeVisible()
  await expect(page.getByRole("button", { name: "Install recipe" })).toBeVisible()
})

test("create agent dialog lists an installed recipe launch profile", async ({ page }) => {
  await page.goto("/")
  await agentsTab(page).click()
  await page.getByRole("button", { name: "New" }).click()
  await expect(page.getByRole("dialog")).toBeVisible()
  await page.getByRole("dialog").getByRole("combobox").first().click()
  await page.getByRole("option", { name: "Codex Agent UI" }).click()
  await expect(page.getByText("./scripts/treer-agent.sh")).toBeVisible()
  await expect(page.getByRole("dialog").getByRole("textbox")).toHaveValue(/codex-agent-ui-\d{4}-/)
  await expect(page.getByRole("button", { name: "Create agent" })).toBeVisible()
})

test("mobile: machine overview hides sidebar and shows back button", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await machinesTab(page).click()
  await workstationRow(page).click()
  await expect(page.getByRole("heading", { name: "workstation" })).toBeVisible()
  await expect(page.locator("aside")).toBeHidden()
  await page.getByRole("button", { name: "Back" }).dispatchEvent("click")
  await expect(page.locator("aside")).toBeVisible()
})

async function openUserMenu(page: Page) {
  await page.getByRole("button", { name: "User menu" }).click()
}

test("user menu contains Settings and Log out, not Edit profile", async ({ page }) => {
  await page.goto("/")
  await openUserMenu(page)
  await expect(page.getByRole("menuitem", { name: "Settings" })).toBeVisible()
  await expect(page.getByRole("menuitem", { name: "Log out" })).toBeVisible()
  await expect(page.getByRole("menuitem", { name: "Edit profile" })).toHaveCount(0)
})

test("opening Settings shows a floating panel with Usage & billing, Account, and General", async ({ page }) => {
  await page.goto("/")
  await openUserMenu(page)
  await page.getByRole("menuitem", { name: "Settings" }).click()
  const settings = page.getByRole("dialog", { name: "Settings" })
  await expect(settings).toBeVisible()
  await expect(settings.getByRole("navigation", { name: "Settings" }).getByRole("button", { name: "Usage & billing" })).toBeVisible()
  await expect(settings.getByRole("navigation", { name: "Settings" }).getByRole("button", { name: "Account" })).toBeVisible()
  await expect(settings.getByRole("navigation", { name: "Settings" }).getByRole("button", { name: "General" })).toBeVisible()
  await expect(page.locator("header").getByText("Settings")).toHaveCount(0)
})

test("Account settings save preferred name via PATCH /api/auth/profile", async ({ page }) => {
  await page.goto("/")
  await openUserMenu(page)
  await page.getByRole("menuitem", { name: "Settings" }).click()

  const settings = page.getByRole("dialog", { name: "Settings" })
  const name = settings.getByLabel("Preferred name")
  const email = settings.getByLabel("Email")
  await expect(name).toHaveValue("Test User")
  await expect(email).toHaveValue("user@example.com")

  await name.fill("Dana Owner")
  const patched = page.waitForRequest((request) => request.url().includes("/api/auth/profile") && request.method() === "PATCH")
  await page.getByRole("button", { name: "Save" }).click()
  const request = await patched
  expect(request.postDataJSON()).toEqual({ email: "user@example.com", preferred_name: "Dana Owner" })

  await expect(page.locator("aside").getByText("Dana Owner")).toBeVisible()
})

test("General settings show theme and English language, and toggle dark class", async ({ page }) => {
  await page.goto("/")
  await openUserMenu(page)
  await page.getByRole("menuitem", { name: "Settings" }).click()
  const settings = page.getByRole("dialog", { name: "Settings" })
  await settings.getByRole("navigation", { name: "Settings" }).getByRole("button", { name: "General" }).click()

  await expect(settings.getByRole("group", { name: "Theme" })).toBeVisible()
  await expect(settings.getByRole("button", { name: "Light" })).toBeVisible()
  await expect(settings.getByRole("button", { name: "Dark" })).toBeVisible()
  await expect(settings.getByLabel("Language")).toHaveText("English")
  await expect(settings.getByText("Only English is available.")).toBeVisible()

  await expect(page.locator("html")).not.toHaveClass(/dark/)
  await page.getByRole("button", { name: "Dark" }).click()
  await expect(page.locator("html")).toHaveClass(/dark/)
  await page.getByRole("button", { name: "Light" }).click()
  await expect(page.locator("html")).not.toHaveClass(/dark/)
})
