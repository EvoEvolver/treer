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

const secondOrganization = {
  organization_id: "org-2",
  name: "Research",
  role: "member" as const,
}

const workspace = {
  workspace_id: "ws-1",
  name: "Demo",
  organization_id: "org-1",
}

const workspaceAccess = {
  workspace_id: "ws-1",
  access_mode: "organization" as const,
  current_role: "owner" as const,
  members: [],
  groups: [],
}

const secondWorkspace = {
  workspace_id: "ws-2",
  name: "Experiments",
  organization_id: "org-2",
}

const build = { version: "0.1.2", git_commit: "abcdef1234567890" }

const machineA = {
  server_id: "srv-a",
  name: "workstation",
  hostname: "workstation.lan",
  root: "/Users/test/worker",
  controller_build: build,
  host_build: build,
  supervision: {
    mode: "foreground" as const,
    fallback_reason: "systemctl --user is unavailable: Failed to connect to bus",
  },
  status: "online",
  available_agents: ["claude"],
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

const appRuntimeAgent = {
  ...agentA,
  agent_id: "appw-1",
  name: "app:Soul Archive",
  kind: "app",
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

const managedApp = {
  app_id: "app-1",
  workspace_id: "ws-1",
  name: "Soul Archive",
  server_id: "srv-a",
  command: "python3",
  args: ["apps/soul/server.py"],
  cwd: ".",
  port: 9420,
  hostname: "soul.demo.internal",
  service_id: "svc-app-1",
  public_url: "https://soul-app1.canary.apps.treer.ai/",
  access: "workspace" as "public" | "workspace",
  desired_state: "running" as const,
  runtime_agent_id: "appw-1",
  restart_count: 1,
  status: "running" as const,
  created_at: NOW,
  created_by: "u1",
  updated_at: NOW,
  updated_by: "u1",
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

const codexProfile = {
  ...recipeProfile,
  profile_id: "alp-codex",
  name: "Codex",
  description: "OpenAI Codex",
  cwd: ".",
  command: "codex",
}

const longScriptProfile = {
  ...recipeProfile,
  profile_id: "alp-long-script",
  name: "Codex + UI",
  description: "Portable Codex UI bootstrap",
  cwd: ".",
  command: "sh",
  args: ["-lc", `set -eu\n${"echo bootstrap-step-with-a-long-value\n".repeat(80)}`],
}

const snapshot = {
  revision: 1,
  workspace,
  servers: [machineA, machineB],
  agents: [agentA, agentB, appRuntimeAgent],
}

const traffic = [
  { window_start: NOW, traffic_class: "virtual_network", source_type: "machine", source_server_id: "srv-a", destination_type: "machine", destination_server_id: "srv-b", payload_bytes: 1500, payload_frames: 12, billable_bytes: 1500, meter_version: 1 },
  { window_start: NOW, traffic_class: "virtual_network", source_type: "machine", source_server_id: "srv-b", destination_type: "machine", destination_server_id: "srv-a", payload_bytes: 750, payload_frames: 9, billable_bytes: 750, meter_version: 1 },
]

function ok(route: Route, body: unknown) {
  return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) })
}

async function mockApi(page: Page) {
  let currentUser = { ...user }
  let currentApp: typeof managedApp = { ...managedApp }
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
    if (path === "/organizations") return ok(route, { organizations: [organization, secondOrganization] })
    if (path === "/organizations/org-1" && route.request().method() === "PATCH") return ok(route, {})
    if (path === "/organizations/org-1/members") return ok(route, { members: [{ user_id: user.user_id, email: user.email, preferred_name: user.preferred_name, role: "owner" }] })
    if (path === "/organizations/org-1/groups") return ok(route, { groups: [{ group_id: "grp-1", organization_id: "org-1", name: "Platform", member_ids: [user.user_id] }] })
    if (path === "/organizations/org-2/members") return ok(route, { members: [{ user_id: user.user_id, email: user.email, preferred_name: user.preferred_name, role: "member" }] })
    if (path === "/organizations/org-2/groups") return ok(route, { groups: [] })
    if (path === "/organizations/org-1/audit-events") return ok(route, { events: [] })
    if (path === "/workspaces") return ok(route, {
      workspaces: url.searchParams.get("organization_id") === "org-1"
        ? [workspace]
        : url.searchParams.get("organization_id") === "org-2" ? [secondWorkspace] : [],
    })
    if (path === "/workspaces/ws-1/snapshot") return ok(route, snapshot)
    if (path === "/workspaces/ws-2/snapshot") return ok(route, { ...snapshot, workspace: secondWorkspace, servers: [], agents: [] })
    if (path === "/workspaces/ws-1/access") return ok(route, { access: workspaceAccess })
    if (path === "/workspaces/ws-2/access") return ok(route, { access: { workspace_id: "ws-2", access_mode: "organization", current_role: "member", members: [], groups: [] } })
    if (path === "/workspaces/ws-1" && route.request().method() === "PATCH") {
      const body = route.request().postDataJSON() as { name: string }
      return ok(route, { workspace: { ...workspace, name: body.name } })
    }
    if (path === "/workspaces/ws-1/agents" && route.request().method() === "POST") return ok(route, {
      agent_id: "ag-installer",
      server_id: "srv-a",
      workspace_id: "ws-1",
      name: "codex-installer",
      kind: "shell",
      status: "starting",
    })
    if (path === "/workspaces/ws-1/virtual-hosts") return ok(route, { hosts: [virtualHost] })
    if (path === "/workspaces/ws-2/virtual-hosts") return ok(route, { hosts: [] })
    if (path === "/workspaces/ws-1/services") return ok(route, { services: [serviceA] })
    if (path === "/workspaces/ws-2/services") return ok(route, { services: [] })
    if (path === "/workspaces/ws-1/traffic") return ok(route, { traffic })
    if (path === "/workspaces/ws-2/traffic") return ok(route, { traffic: [] })
    if (path === "/workspaces/ws-1/launch-profiles") return ok(route, { profiles: [recipeProfile, codexProfile, longScriptProfile] })
    if (path === "/workspaces/ws-2/launch-profiles") return ok(route, { profiles: [] })
    if (path === "/workspaces/ws-1/apps" && route.request().method() === "GET") return ok(route, { apps: [currentApp] })
    if (path === "/workspaces/ws-1/apps/app-1/access" && route.request().method() === "PATCH") {
      const body = route.request().postDataJSON() as { access: "public" | "workspace" }
      currentApp = { ...currentApp, access: body.access }
      return ok(route, { app: currentApp })
    }
    if (/^\/workspaces\/ws-1\/apps\/[^/]+\/(start|stop|restart)$/.test(path) && route.request().method() === "POST") return ok(route, { app: currentApp })
    if (/^\/workspaces\/ws-1\/apps\/[^/]+$/.test(path) && route.request().method() === "DELETE") return ok(route, {})
    if (path === "/workspaces/ws-2/apps" && route.request().method() === "GET") return ok(route, { apps: [] })

    return route.fulfill({ status: 404, contentType: "application/json", body: "{}" })
  })
}

test.beforeEach(async ({ page }) => {
  await mockApi(page)
})

// Helpers — machine entries in workspace settings are <button>s whose accessible
// name includes the machine name, root path and build label.
const workstationRow = (page: Page) => page.getByRole("button", { name: /^workstation / })
const cloudboxRow = (page: Page) => page.getByRole("button", { name: /^cloudbox / })
const agentsTab = (page: Page) => page.getByRole("tab", { name: /Agents/ })
const appsTab = (page: Page) => page.getByRole("tab", { name: /Apps/ })

async function openWorkspaceSettings(page: Page) {
  await page.getByRole("button", { name: "Workspace settings" }).click()
  await expect(page.getByRole("heading", { name: "Launch profiles" })).toBeVisible()
}

test("opens app, shows org, workspace, Apps and agents", async ({ page }) => {
  await page.goto("/")
  await expect(page.locator("aside").getByText("Acme")).toBeVisible()
  await expect(page.getByRole("combobox", { name: "Workspace" })).toHaveText("Demo")
  await expect.poll(() => new URL(page.url()).pathname).toBe("/orgs/org-1/workspaces/ws-1")
  await expect(page).toHaveTitle("Acme / Demo")
  await expect(page.locator("aside [role=tab]").nth(0)).toContainText("Agents")
  await expect(page.locator("aside [role=tab]").nth(1)).toContainText("Apps")

  await appsTab(page).click()
  await expect(page.locator("aside").getByRole("button", { name: "Open Soul Archive settings" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Apps" })).toBeVisible()

  await agentsTab(page).click()
  await expect(page.getByRole("button", { name: /^api-server / })).toBeVisible()
  await expect(page.getByRole("button", { name: /^worker / })).toBeVisible()
  await expect(page.getByRole("button", { name: /^app:Soul Archive / })).toHaveCount(0)
})

test("restores organization and workspace from the URL after reload", async ({ page }) => {
  await page.goto("/orgs/org-2/workspaces/ws-2?source=bookmark")
  await expect(page.getByRole("combobox", { name: "Organization" })).toHaveText("Research")
  await expect(page.getByRole("combobox", { name: "Workspace" })).toHaveText("Experiments")
  await expect.poll(() => new URL(page.url()).searchParams.get("source")).toBe("bookmark")
  await expect(page).toHaveTitle("Research / Experiments")

  await page.reload()

  await expect(page.getByRole("combobox", { name: "Organization" })).toHaveText("Research")
  await expect(page.getByRole("combobox", { name: "Workspace" })).toHaveText("Experiments")
  await expect.poll(() => new URL(page.url()).pathname).toBe("/orgs/org-2/workspaces/ws-2")
  await expect(page).toHaveTitle("Research / Experiments")
})

test("late snapshots from the previous workspace cannot replace the current inventory", async ({ page }) => {
  let releaseFirstSnapshot = () => {}
  let firstSnapshotStarted = false
  const firstSnapshotBlocked = new Promise<void>((resolve) => { releaseFirstSnapshot = resolve })
  let firstSnapshotResponded = () => {}
  const firstSnapshotResponse = new Promise<void>((resolve) => { firstSnapshotResponded = resolve })
  await page.route("**/api/workspaces/ws-1/snapshot", async (route) => {
    firstSnapshotStarted = true
    await firstSnapshotBlocked
    await ok(route, snapshot)
    firstSnapshotResponded()
  })

  await page.goto("/")
  await expect.poll(() => firstSnapshotStarted).toBe(true)
  await page.getByRole("combobox", { name: "Organization" }).click()
  await page.getByRole("option", { name: "Research" }).click()
  await expect(page.getByRole("combobox", { name: "Workspace" })).toHaveText("Experiments")
  await expect(page).toHaveTitle("Research / Experiments")

  releaseFirstSnapshot()
  await firstSnapshotResponse
  await page.waitForTimeout(100)
  await expect(page.getByRole("tab", { name: /Apps 0/ })).toBeVisible()
  await expect(page.getByRole("tab", { name: /Agents 0/ })).toBeVisible()
  await expect(workstationRow(page)).toHaveCount(0)
  await expect(page.getByRole("button", { name: /^api-server / })).toHaveCount(0)
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

test("new agent sends an empty workspace to machine setup", async ({ page }) => {
  await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, {
    ...snapshot,
    servers: [],
    agents: [],
  }))
  await page.goto("/")

  const addMachinePrompt = page.getByRole("button", { name: "Set up a machine before creating an agent" })
  await expect(addMachinePrompt).toBeEnabled()
  await addMachinePrompt.click()

  await expect(page.getByRole("heading", { name: "Machines" })).toBeVisible()
  await expect(page.locator("section[data-tour='workspace-machines']").getByRole("button", { name: "Add", exact: true })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Create agent" })).toHaveCount(0)
})

test("organization actions open a settings page with inline management", async ({ page }) => {
  await page.goto("/")
  const organizationSettings = page.getByRole("button", { name: "Organization settings" })
  await expect(organizationSettings.locator("svg")).toHaveClass(/lucide-ellipsis/)
  await organizationSettings.click()

  await expect(page.getByRole("heading", { name: "Acme", exact: true })).toBeVisible()
  await expect(page.getByRole("heading", { name: "General" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Workspaces" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Members" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Groups" })).toBeVisible()
  await expect(page.getByText("Platform", { exact: true })).toBeVisible()
  await expect(page.getByRole("button", { name: "New organization" })).toBeVisible()
  await expect(page.getByRole("button", { name: "Open audit" })).toBeVisible()

  await expect(page.getByLabel("Organization name")).toBeEnabled()
  await page.getByRole("button", { name: "Select Demo workspace" }).click()
  await expect(page.getByRole("heading", { name: "General" })).toBeHidden()
})

test("mobile organization settings hide the sidebar and fit the viewport", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await page.getByRole("button", { name: "Organization settings" }).click()

  await expect(page.locator("aside")).toBeHidden()
  await expect(page.getByRole("heading", { name: "Acme", exact: true })).toBeVisible()
  await expect(page.getByRole("button", { name: "Back" })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

test("audit view presents versioned traffic as billable usage", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("button", { name: "Organization settings" }).click()
  await page.getByRole("button", { name: "Open audit" }).click()

  await expect(page.getByText("Billable usage")).toBeVisible()
  await expect(page.getByText("Data frames")).toBeVisible()
  await expect(page.getByText("Machine", { exact: true })).toHaveCount(2)
})

test("workspace settings contain Machines and launch profiles without a separate views menu", async ({ page }) => {
  await page.goto("/")
  await expect(page.getByRole("button", { name: "Workspace views" })).toHaveCount(0)
  await openWorkspaceSettings(page)
  await expect(page.getByRole("heading", { name: "Machines" })).toBeVisible()
  await expect(workstationRow(page)).toBeVisible()
  await expect(page.getByText("Codex Agent UI", { exact: true })).toBeVisible()
  await expect(page.getByRole("button", { name: "New profile" })).toBeVisible()
  await expect(appsTab(page)).toBeVisible()
})

test("workspace controls open a settings page with inline rename", async ({ page }) => {
  await page.goto("/")
  const workspaceSettings = page.getByRole("button", { name: "Workspace settings" })
  await expect(workspaceSettings).toHaveCount(1)
  await expect(workspaceSettings.locator("svg")).toHaveClass(/lucide-ellipsis/)
  await expect(page.getByRole("button", { name: "Rename workspace" })).toHaveCount(0)
  await expect(page.getByRole("button", { name: "Delete workspace" })).toHaveCount(0)

  await openWorkspaceSettings(page)
  await expect(page.getByRole("heading", { name: "Demo", exact: true })).toBeVisible()
  await expect(page.getByText("ws-1", { exact: true })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Machines" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Launch profiles" })).toBeVisible()

  const name = page.getByLabel("Workspace name")
  await name.fill("Platform")
  const renaming = page.waitForRequest((request) => request.url().endsWith("/api/workspaces/ws-1") && request.method() === "PATCH")
  await page.getByRole("button", { name: "Save" }).click()
  const request = await renaming
  expect(request.postDataJSON()).toEqual({ name: "Platform" })
  await expect(page).toHaveTitle("Acme / Platform")
  await expect(page.getByRole("heading", { name: "Platform", exact: true })).toBeVisible()
})

test("managed App settings control access and lifecycle without creation or Network UI", async ({ page }) => {
  await page.goto("/")
  await appsTab(page).click()

  await expect(page.locator("main > section").getByText("Soul Archive")).toBeVisible()
  await expect(page.getByText("https://soul-app1.canary.apps.treer.ai/", { exact: true })).toBeVisible()
  await expect(page.getByRole("button", { name: /New App|New$/ })).toHaveCount(0)
  await expect(page.getByRole("heading", { name: "Network" })).toHaveCount(0)
  await expect(page.getByText(/_human/)).toHaveCount(0)
  await page.getByRole("button", { name: "Open Soul Archive settings" }).first().click()
  await expect(page.getByRole("heading", { name: "Soul Archive" })).toBeVisible()
  await expect(page.getByRole("combobox", { name: "App access" })).toHaveText("Workspace session required")

  const publishing = page.waitForRequest((request) => request.url().includes("/apps/app-1/access") && request.method() === "PATCH")
  await page.getByRole("combobox", { name: "App access" }).click()
  await page.getByRole("option", { name: "Public · no authentication" }).click()
  const publishRequest = await publishing
  expect(publishRequest.postDataJSON()).toEqual({ access: "public" })
  await expect(page.getByText("Anyone with this URL can access the App without a Treer session.")).toBeVisible()

  await page.evaluate(() => {
    window.open = ((url?: string | URL) => {
      document.documentElement.dataset.lastOpenedUrl = String(url)
      return null
    }) as typeof window.open
  })
  await page.getByRole("button", { name: "Open Soul Archive", exact: true }).click()
  await expect.poll(() => page.locator("html").getAttribute("data-last-opened-url")).toBe("https://soul-app1.canary.apps.treer.ai/")
  const restarting = page.waitForRequest((request) => request.url().includes("/apps/app-1/restart") && request.method() === "POST")
  await page.getByRole("button", { name: "Restart" }).click()
  await restarting
})

test("mobile App settings hide the sidebar and fit the viewport", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await appsTab(page).click()
  await page.getByRole("button", { name: "Open Soul Archive settings" }).first().click()

  await expect(page.getByRole("heading", { name: "Soul Archive" })).toBeVisible()
  await expect(page.locator("aside")).toBeHidden()
  await expect(page.getByRole("button", { name: "Back" })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

test("clicking a machine opens an overview with identity, agents, services, virtual hosts, and traffic", async ({ page }) => {
  await page.goto("/")
  await openWorkspaceSettings(page)
  await workstationRow(page).click()

  // Identity
  await expect(page.getByRole("heading", { name: "workstation" })).toBeVisible()
  await expect(page.getByText("srv-a")).toBeVisible()
  await expect(page.getByText("workstation.lan")).toBeVisible()
  await expect(page.getByTitle("/Users/test/worker")).toBeVisible()
  await expect(page.getByText("foreground", { exact: true })).toBeVisible()
  await expect(page.getByText("Foreground fallback.")).toBeVisible()
  await expect(page.getByText(/Failed to connect to bus/)).toBeVisible()

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
  await expect(page.getByText("Routed out")).toBeVisible()
  await expect(page.getByText("Routed in")).toBeVisible()

  await page.getByRole("button", { name: "Close" }).click()
  await expect(page.getByRole("heading", { name: "workstation" })).toBeHidden()
})

test("workspace members can see machine traffic without audit permission", async ({ page }) => {
  await page.route(/\/api\/organizations$/, (route) => ok(route, {
    organizations: [{ ...organization, role: "member" }],
  }))
  await page.route("**/api/workspaces/ws-1/traffic?hours=24", (route) => ok(route, {
    traffic: traffic.map((item) => ({
      window_start: item.window_start,
      source_server_id: item.source_server_id,
      destination_server_id: item.destination_server_id,
      payload_bytes: item.payload_bytes,
      payload_frames: item.payload_frames,
    })),
  }))
  await page.goto("/")
  await openWorkspaceSettings(page)
  await workstationRow(page).click()

  await expect(page.getByText("Routed out")).toBeVisible()
  await expect(page.getByText("1.46 KB")).toBeVisible()
  await expect(page.getByText("750 B")).toBeVisible()
})

test("offline machines show copyable recovery commands with the workspace id", async ({ page }) => {
  await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, {
    revision: 1,
    workspace,
    servers: [
      { ...machineA, status: "offline" },
      { ...machineB, hostname: "workstation.lan", name: "workstation", status: "offline", labels: { "treer.listen": "127.0.0.1:8794" } },
    ],
    agents: [agentA, agentB],
  }))
  await page.goto("/")
  await openWorkspaceSettings(page)
  await expect(page.getByText("workstation · srv-b :8794")).toBeVisible()
  await page.getByRole("button", { name: /^workstation / }).first().click()
  await expect(page.getByText("treer-agent-server service --workspace ws-1 restart-controller")).toBeVisible()
  await expect(page.getByRole("button", { name: "Copy restart-controller" })).toBeVisible()
  await expect(page.getByRole("button", { name: "Copy start" })).toBeVisible()
})

test("machine overview only shows agents for the selected machine", async ({ page }) => {
  await page.goto("/")
  await openWorkspaceSettings(page)
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
  await openWorkspaceSettings(page)
  await workstationRow(page).click()
  await page.getByRole("button", { name: "Terminal", exact: true }).click()

  // Returns to the terminal main view; header shows agent name
  await expect(page.locator("header").getByText("api-server")).toBeVisible()
})

test("create agent dialog can install a git recipe", async ({ page }) => {
  await page.goto("/")
  await agentsTab(page).click()
  await page.getByRole("button", { name: "New" }).click()
  await expect(page.getByRole("dialog")).toBeVisible()
  await page.getByRole("dialog").getByRole("combobox", { name: "Launch" }).click()
  await page.getByRole("option", { name: "Install recipe" }).click()
  await expect(page.getByPlaceholder("https://github.com/example/recipe.git")).toBeVisible()
  await expect(page.getByText("Only agents already installed on the selected machine can run a recipe.")).toBeVisible()
  await expect(page.getByRole("button", { name: "Install recipe" })).toBeVisible()
})

test("create agent dialog can install a missing CLI directly from the launch list", async ({ page }) => {
  await page.goto("/")
  await agentsTab(page).click()
  await page.getByRole("button", { name: "New" }).click()
  await expect(page.getByRole("dialog")).toBeVisible()
  const installRequest = page.waitForRequest((request) => request.method() === "POST" && new URL(request.url()).pathname === "/api/workspaces/ws-1/agents")
  await page.getByRole("dialog").getByRole("combobox", { name: "Launch" }).click()
  await page.getByRole("option", { name: "Install Codex", exact: true }).click()
  const request = await installRequest
  expect(request.postDataJSON()).toMatchObject({
    server_id: "srv-a",
    kind: "shell",
    cwd: ".",
    args: ["bash", "-lc", expect.stringContaining("https://chatgpt.com/codex/install.sh")],
  })
  await expect(page.getByRole("dialog")).toBeHidden()
})

test("create agent dialog lists an installed recipe launch profile", async ({ page }) => {
  await page.goto("/")
  await agentsTab(page).click()
  await page.getByRole("button", { name: "New" }).click()
  await expect(page.getByRole("dialog")).toBeVisible()
  await page.getByRole("dialog").getByRole("combobox", { name: "Launch" }).click()
  await page.getByRole("option", { name: "Codex Agent UI" }).click()
  await expect(page.getByText("./scripts/treer-agent.sh")).toBeVisible()
  await expect(page.getByRole("dialog").getByRole("textbox")).toHaveValue(/codex-agent-ui-\d{4}-/)
  await expect(page.getByRole("button", { name: "Create agent" })).toBeVisible()
})

test("create agent dialog contains a long profile command on narrow screens", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await agentsTab(page).click()
  await page.getByRole("button", { name: "New" }).click()
  const dialog = page.getByRole("dialog")
  await dialog.getByRole("combobox", { name: "Launch" }).click()
  await page.getByRole("option", { name: "Codex + UI" }).click()

  const dialogBox = await dialog.boundingBox()
  const commandBox = await dialog.locator("code").boundingBox()
  expect(dialogBox).not.toBeNull()
  expect(commandBox).not.toBeNull()
  expect(dialogBox!.x).toBeGreaterThanOrEqual(0)
  expect(dialogBox!.x + dialogBox!.width).toBeLessThanOrEqual(390)
  expect(commandBox!.x).toBeGreaterThanOrEqual(dialogBox!.x)
  expect(commandBox!.x + commandBox!.width).toBeLessThanOrEqual(dialogBox!.x + dialogBox!.width)
})

test("mobile: machine overview hides sidebar and shows back button", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await openWorkspaceSettings(page)
  await workstationRow(page).click()
  await expect(page.getByRole("heading", { name: "workstation" })).toBeVisible()
  await expect(page.locator("aside")).toBeHidden()
  await page.getByRole("button", { name: "Back" }).dispatchEvent("click")
  await expect(page.getByRole("heading", { name: "Launch profiles" })).toBeVisible()
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
  await expect(settings.getByText("Control plane")).toHaveCount(0)
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

test("manager can delete a workspace and the app recovers to no workspace", async ({ page }) => {
  let workspaces = [workspace]
  const deletedPaths: string[] = []
  await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, {
    ...snapshot,
    servers: [],
    agents: [],
  }))
  await page.route(/\/api\/workspaces/, (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api/, "")
    if (path === "/workspaces/ws-1" && route.request().method() === "DELETE") {
      deletedPaths.push(url.pathname)
      workspaces = []
      return ok(route, {
        workspace_id: "ws-1",
        organization_id: "org-1",
        name: "Demo",
        machine_count: 0,
        agent_count: 2,
        app_count: 1,
      })
    }
    if (path === "/workspaces" && route.request().method() === "GET") {
      return ok(route, {
        workspaces: workspaces.filter(
          (item) => item.organization_id === url.searchParams.get("organization_id"),
        ),
      })
    }
    return route.fallback()
  })

  await page.goto("/")
  await expect(page.getByRole("combobox", { name: "Workspace" })).toHaveText("Demo")

  await page.getByRole("button", { name: "Workspace settings" }).click()
  await page.getByRole("button", { name: "Delete workspace" }).click()
  const dialog = page.getByRole("dialog")
  await expect(dialog.getByRole("heading", { name: "Delete workspace" })).toBeVisible()
  await expect(dialog.getByText("Delete Demo?")).toBeVisible()
  await expect(dialog.getByText("historical traffic and messages are retained")).toBeVisible()
  await expect(dialog.getByText("This cannot be undone.")).toBeVisible()

  await dialog.getByRole("button", { name: "Delete workspace" }).click()
  await expect(deletedPaths).toEqual(["/api/workspaces/ws-1"])
  await expect(page.getByRole("combobox", { name: "Workspace" })).toHaveText("No workspace")
  await expect.poll(() => new URL(page.url()).pathname).toBe("/orgs/org-1")
})

test("workspace deletion is blocked until every machine is deleted", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("button", { name: "Workspace settings" }).click()
  await expect(page.getByText("Delete all 2 machines before deleting this workspace.")).toBeVisible()
  await expect(page.getByRole("button", { name: "Delete workspace" })).toBeDisabled()
  await expect(page.getByRole("dialog")).toHaveCount(0)
})
