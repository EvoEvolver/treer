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
  path: "/api-server",
  kind: "command",
  status: "running",
}

const agentB = {
  agent_id: "ag-2",
  server_id: "srv-b",
  name: "worker",
  path: "/worker",
  kind: "terminal",
  status: "running",
}

const appRuntimeAgent = {
  ...agentA,
  agent_id: "appw-1",
  name: "app:Soul Archive",
  path: "/Soul Archive",
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

const defaultPolicyApp = {
  ...managedApp,
  app_id: "app-policy",
  name: "Treer Policy",
  command: "python3",
  args: ["treer-policy.py", "--port", "8787"],
  port: 8787,
  hostname: "policy.internal",
  service_id: "svc-policy",
  public_url: "https://treer-policy.canary.apps.treer.ai/",
  runtime_agent_id: "appw-policy",
  restart_count: 0,
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
  const trees: Record<string, { folders: { folder_id: string; name: string; parent_id: string | null }[]; placements: { agent_id: string; folder_id: string | null; position: number }[] }> = {}
  let currentUser = { ...user }
  let currentApp: typeof managedApp = { ...managedApp }
  let appDeleted = false
  let policyInstalled = false
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/events$/, () => {})
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/agents\/[^/]+\/terminal(?:\?.*)?$/, () => {})
  await page.route("**/api/**", async (route: Route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api/, "")
    const treeMatch = path.match(/^\/workspaces\/([^/]+)\/workload-tree(?:\/(folders|placements)(?:\/([^/]+))?)?$/)
    if (treeMatch) {
      const [, ws, resource, id] = treeMatch
      const tree = trees[ws] ??= { folders: [], placements: [] }
      const method = route.request().method()
      if (method === "GET") return ok(route, tree)
      const body = method === "DELETE" ? {} : route.request().postDataJSON()
      if (resource === "folders" && method === "POST") { const folder = { folder_id: `folder-${tree.folders.length + 1}`, name: body.name, parent_id: body.parent_id ?? null }; tree.folders.push(folder); return ok(route, { folder }) }
      if (resource === "folders" && method === "PATCH") { const folder = tree.folders.find((entry) => entry.folder_id === id)!; Object.assign(folder, body); return ok(route, { folder }) }
      if (resource === "folders" && method === "DELETE") { tree.folders = tree.folders.filter((entry) => entry.folder_id !== id); tree.folders.forEach((entry) => { if (entry.parent_id === id) entry.parent_id = null }); tree.placements.forEach((entry) => { if (entry.folder_id === id) entry.folder_id = null }); return ok(route, {}) }
      if (resource === "placements") {
        tree.placements = tree.placements.filter((entry) => entry.agent_id !== id)
        for (const folderId of new Set(tree.placements.map((entry) => entry.folder_id))) {
          tree.placements.filter((entry) => entry.folder_id === folderId).sort((a, b) => a.position - b.position).forEach((entry, position) => { entry.position = position })
        }
        const siblings = tree.placements.filter((entry) => entry.folder_id === body.folder_id)
        const position = Math.min(body.position, siblings.length)
        siblings.forEach((entry) => { if (entry.position >= position) entry.position++ })
        const placement = { agent_id: id!, folder_id: body.folder_id, position }
        tree.placements.push(placement)
        return ok(route, { placement })
      }
    }

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
    if (path === "/workspaces/ws-1/policy-provider/default-app" && route.request().method() === "POST") {
      policyInstalled = true
      return ok(route, {
        provider: { workspace_id: "ws-1", app_id: defaultPolicyApp.app_id, service_id: defaultPolicyApp.service_id, failure_mode: "fail_closed", max_stale_seconds: 300, revision_hint: 1, updated_at: NOW, updated_by: user.user_id },
        app: defaultPolicyApp,
        cache: {},
      })
    }
    if (path === "/workspaces/ws-1/policy-provider" && route.request().method() === "GET") return ok(route, policyInstalled ? {
      provider: { workspace_id: "ws-1", app_id: defaultPolicyApp.app_id, service_id: defaultPolicyApp.service_id, failure_mode: "fail_closed", max_stale_seconds: 300, revision_hint: 1, updated_at: NOW, updated_by: user.user_id },
      app: defaultPolicyApp,
      cache: {},
    } : {
      fallback: { kind: "treer_default", name: "Treer Default", mode: "monitor", effect: "allow" },
      cache: {},
    })
    if (path === "/workspaces/ws-2/policy-provider" && route.request().method() === "GET") return ok(route, {
      fallback: { kind: "treer_default", name: "Treer Default", mode: "monitor", effect: "allow" },
      cache: {},
    })
    if (path === "/workspaces/ws-1/apps" && route.request().method() === "GET") return ok(route, { apps: appDeleted ? [] : [currentApp] })
    if (path === "/workspaces/ws-1/apps/app-1/access" && route.request().method() === "PATCH") {
      const body = route.request().postDataJSON() as { access: "public" | "workspace" }
      currentApp = { ...currentApp, access: body.access }
      return ok(route, { app: currentApp })
    }
    if (/^\/workspaces\/ws-1\/apps\/[^/]+\/(start|stop|restart)$/.test(path) && route.request().method() === "POST") return ok(route, { app: currentApp })
    if (/^\/workspaces\/ws-1\/apps\/[^/]+$/.test(path) && route.request().method() === "DELETE") { appDeleted = true; return ok(route, {}) }
    if (path === "/workspaces/ws-2/apps" && route.request().method() === "GET") return ok(route, { apps: [] })

    return route.fulfill({ status: 404, contentType: "application/json", body: "{}" })
  })
}

test.beforeEach(async ({ page }) => {
  await mockApi(page)
})

test.describe('resizable tree sidebar', () => {
  test.use({ viewport: { width: 1280, height: 900 } })
  const key = 'treer:tree-width:v1:u1'
  const separator = (page: Page) => page.getByRole('separator', { name: 'Resize tree sidebar' })
  async function drag(page: Page, delta: number) {
    const box = (await separator(page).boundingBox())!
    await page.mouse.move(box.x + box.width / 2, box.y + 200)
    await page.mouse.down()
    await page.mouse.move(box.x + box.width / 2 + delta, box.y + 200, { steps: 8 })
  }
  test('drag persists across refresh/workspaces, protects terminal, and double click resets', async ({ page }, testInfo) => {
    const resizeColumns: number[] = []
    let initialColumns = 0
    await page.routeWebSocket(/\/agents\/[^/]+\/terminal(?:\?.*)?$/, (ws) => {
      initialColumns = Number(new URL(ws.url()).searchParams.get('cols'))
      ws.onMessage((message) => { if (typeof message === 'string') { const control = JSON.parse(message); if (control.type === 'resize') resizeColumns.push(control.cols) } })
      ws.send(JSON.stringify({ type: 'ready', stream_epoch: 'resize', revision: 0, replay_chunks: 0 }))
    })
    await page.goto('/')
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '272')
    await page.getByRole('button', { name: /^api-server / }).click()
    await expect.poll(() => initialColumns).toBeGreaterThan(0)
    await drag(page, 96)
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '368')
    await expect(page.locator('main > section')).toHaveCSS('pointer-events', 'none')
    await page.mouse.up()
    await expect(page.locator('main > section')).toHaveCSS('pointer-events', 'auto')
    await expect.poll(() => resizeColumns.at(-1)!).toBeLessThan(initialColumns)
    const expandedColumns = resizeColumns.at(-1)!
    await page.getByRole('button', { name: 'Collapse tree sidebar', exact: true }).click()
    await expect.poll(() => resizeColumns.at(-1)!).toBeGreaterThan(expandedColumns)
    await page.getByRole('button', { name: 'Expand tree sidebar', exact: true }).click()
    await expect.poll(() => resizeColumns.at(-1)!).toBe(expandedColumns)
    expect(await page.evaluate((key) => localStorage.getItem(key), key)).toBe('368')
    await page.screenshot({ path: testInfo.outputPath('tree-resizer-desktop.png') })
    await page.reload()
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '368')
    await page.goto('/orgs/org-2/workspaces/ws-2')
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '368')
    await separator(page).dblclick()
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '272')
    await page.reload()
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '272')
  })
  test('collapse is keyboard accessible and persists without losing width or mobile navigation', async ({ page }, testInfo) => {
    await page.goto('/')
    await separator(page).press('ArrowRight')
    const collapse = page.getByRole('button', { name: 'Collapse tree sidebar', exact: true })
    await collapse.focus()
    await page.keyboard.press('Enter')
    const expand = page.getByRole('button', { name: 'Expand tree sidebar', exact: true })
    await expect(expand).toBeFocused()
    await expect(expand).toHaveAttribute('aria-expanded', 'false')
    await expect(page.locator('#tree-sidebar')).toBeHidden()
    expect(await page.evaluate(() => localStorage.getItem('treer:tree-collapsed:v1:u1'))).toBe('true')
    expect(await page.evaluate((key) => localStorage.getItem(key), key)).toBe('288')
    await page.screenshot({ path: testInfo.outputPath('tree-collapsed-desktop.png') })
    await page.reload()
    await expect(expand).toBeVisible()
    await expect(page.locator('#tree-sidebar')).toBeHidden()
    await page.goto('/orgs/org-2/workspaces/ws-2')
    await expect(expand).toBeVisible()
    await page.setViewportSize({ width: 390, height: 844 })
    await expect(expand).toBeHidden()
    await expect(page.locator('#tree-sidebar')).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: testInfo.outputPath('tree-collapsed-preference-mobile.png') })
    await page.setViewportSize({ width: 1280, height: 900 })
    await expand.focus()
    await page.keyboard.press('Space')
    await expect(collapse).toHaveAttribute('aria-expanded', 'true')
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '288')
    await page.reload()
    await expect(collapse).toBeVisible()
    await expect(page.locator('#tree-sidebar')).toBeVisible()
  })
  test('keyboard exposes separator values, clamps on resize and stays hidden at 390px', async ({ page }, testInfo) => {
    await page.goto('/')
    const bar = separator(page)
    await expect(bar).toHaveAttribute('aria-orientation', 'vertical')
    await bar.focus()
    await page.keyboard.press('ArrowRight')
    await expect(bar).toHaveAttribute('aria-valuenow', '288')
    await page.keyboard.press('ArrowLeft')
    await expect(bar).toHaveAttribute('aria-valuenow', '272')
    await page.keyboard.press('Home')
    await expect(bar).toHaveAttribute('aria-valuenow', '240')
    await page.keyboard.press('End')
    await expect(bar).toHaveAttribute('aria-valuenow', '480')
    await page.setViewportSize({ width: 800, height: 900 })
    await expect(bar).toHaveAttribute('aria-valuemax', '360')
    await expect(bar).toHaveAttribute('aria-valuenow', '360')
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.setViewportSize({ width: 390, height: 844 })
    await expect(bar).toHaveCount(0)
    await expect(page.locator('aside')).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: testInfo.outputPath('tree-resizer-mobile.png') })
  })
  for (const failure of ['getter', 'read', 'write']) {
    test(`storage ${failure} failure retains usable default and resizing`, async ({ page }) => {
      await page.addInitScript((failure) => {
        if (failure === 'getter') Object.defineProperty(window, 'localStorage', { get() { throw new Error('disabled') } })
        else Object.defineProperty(Storage.prototype, failure === 'read' ? 'getItem' : 'setItem', { value() { throw new Error('disabled') } })
      }, failure)
      await page.goto('/')
      await expect(separator(page)).toHaveAttribute('aria-valuenow', '272')
      await separator(page).press('ArrowRight')
      await expect(separator(page)).toHaveAttribute('aria-valuenow', '288')
      await page.getByRole('button', { name: 'Collapse tree sidebar', exact: true }).click()
      await expect(page.locator('#tree-sidebar')).toBeHidden()
      await page.getByRole('button', { name: 'Expand tree sidebar', exact: true }).click()
      await expect(page.locator('#tree-sidebar')).toBeVisible()
    })
  }
  test('width belongs to the signed in user and invalid cache uses default', async ({ page }) => {
    await page.addInitScript(() => { localStorage.setItem('treer:tree-width:v1:other-user', '480'); localStorage.setItem('treer:tree-width:v1:u1', 'invalid'); localStorage.setItem('treer:tree-collapsed:v1:other-user', 'true'); localStorage.setItem('treer:tree-collapsed:v1:u1', 'invalid') })
    await page.goto('/')
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '272')
    await separator(page).press('ArrowRight')
    expect(await page.evaluate(() => localStorage.getItem('treer:tree-width:v1:other-user'))).toBe('480')
    await page.getByRole('button', { name: 'Collapse tree sidebar', exact: true }).click()
    await page.getByRole('button', { name: 'Expand tree sidebar', exact: true }).click()
    expect(await page.evaluate(() => localStorage.getItem('treer:tree-collapsed:v1:other-user'))).toBe('true')
  })
  test('touch pointer drag resizes on a large touch screen', async ({ page, context }) => {
    await page.goto('/')
    const box = (await separator(page).boundingBox())!
    const session = await context.newCDPSession(page)
    const start = { x: box.x + box.width / 2, y: box.y + 200 }
    await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [start] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ ...start, x: start.x + 80 }] })
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '352')
    await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await expect(page.locator('main > section')).toHaveCSS('pointer-events', 'auto')
  })
  test('losing window focus ends protection and the terminal remains usable', async ({ page }) => {
    await page.goto('/')
    await drag(page, 48)
    await expect(page.locator('main > section')).toHaveCSS('pointer-events', 'none')
    await page.evaluate(() => dispatchEvent(new Event('blur')))
    await expect(page.locator('main > section')).toHaveCSS('pointer-events', 'auto')
    await page.mouse.up()
    await separator(page).press('Home')
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '240')
  })
})

for (const mobile of [false, true]) {
  test.describe(`startup command on ${mobile ? "mobile" : "desktop"}`, () => {
    test.use({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 }, hasTouch: mobile, isMobile: mobile })
    test("shows current argv and saves one atomic restart with the same identity", async ({ page }) => {
      let replacements = 0
      await page.route("**/agents/ag-1/admin/startup", (route) => ok(route, { ...agentA, cwd: "repo", command: "codex", args: ["resume", "original-thread", "literal value"], enabled: true, generation: 3 }))
      await page.route("**/agents/ag-1/restart-with-startup", async (route) => {
        replacements++
        expect(route.request().postDataJSON()).toEqual({ cwd: "other repo", command: "codex", args: ["--no-daemon", "resume", "original-thread", "two words"] })
        await ok(route, { ...agentA, run_generation: 4 })
      })
      await page.goto("/")
      await page.getByRole("button", { name: "Actions for api-server", exact: true }).click()
      await page.getByRole("menuitem", { name: "Edit startup command", exact: true }).click()
      const dialog = page.getByRole("dialog")
      await expect(dialog.getByRole("textbox", { name: "Startup command", exact: true })).toHaveValue("codex resume original-thread 'literal value'")
      await expect(dialog.getByRole("textbox", { name: "Startup working directory" })).toHaveValue("repo")
      await dialog.getByRole("textbox", { name: "Startup command", exact: true }).fill("codex --no-daemon resume original-thread 'two words'")
      await dialog.getByRole("textbox", { name: "Startup working directory" }).fill("other repo")
      await dialog.getByRole("button", { name: "Save and restart" }).click()
      await expect(dialog).not.toBeVisible()
      await expect(page.getByRole("status")).toContainText("Saved startup command and restarted /api-server")
      expect(replacements).toBe(1)
      await page.getByRole("button", { name: "Actions for Soul Archive", exact: true }).click()
      await expect(page.getByRole("menuitem", { name: "Edit startup command", exact: true })).toHaveCount(0)
    })
    test("keeps errors visible and never restarts after an unsupported read", async ({ page }) => {
      let replacements = 0
      await page.route("**/agents/ag-1/admin/startup", (route) => route.fulfill({ status: 400, contentType: "application/json", body: JSON.stringify({ error: { code: "unsupported_command", message: "Controller requires agent.startup-admin.v1" } }) }))
      await page.route("**/agents/ag-1/restart-with-startup", (route) => { replacements++; return ok(route, {}) })
      await page.goto("/")
      await page.getByRole("button", { name: "Actions for api-server", exact: true }).click()
      await page.getByRole("menuitem", { name: "Edit startup command", exact: true }).click()
      await expect(page.getByRole("dialog").getByRole("alert")).toContainText("agent.startup-admin.v1")
      await expect(page.getByRole("button", { name: "Save and restart" })).toBeDisabled()
      expect(replacements).toBe(0)
    })
    test("preserves multiline shell argv when saving the displayed command", async ({ page }) => {
      const script = "set -eu\necho '$HOME'\nexec sleep 120"
      await page.route("**/agents/ag-1/admin/startup", (route) => ok(route, { command: "sh", args: ["-lc", script], cwd: "." }))
      const saved = page.waitForRequest("**/agents/ag-1/restart-with-startup")
      await page.route("**/agents/ag-1/restart-with-startup", (route) => ok(route, agentA))
      await page.goto("/")
      await page.getByRole("button", { name: "Actions for api-server", exact: true }).click()
      await page.getByRole("menuitem", { name: "Edit startup command", exact: true }).click()
      await expect(page.getByRole("textbox", { name: "Startup command", exact: true })).toHaveValue(/exec sleep 120/)
      await page.getByRole("button", { name: "Save and restart" }).click()
      expect((await saved).postDataJSON()).toEqual({ command: "sh", args: ["-lc", script], cwd: "." })
    })
    test("rejects shell operators and preserves the editor on denied save", async ({ page }) => {
      let replacements = 0
      await page.route("**/agents/ag-1/admin/startup", (route) => ok(route, { command: "sh", args: [], cwd: "." }))
      await page.route("**/agents/ag-1/restart-with-startup", (route) => { replacements++; return route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: { code: "policy_denied", message: "startup management denied" } }) }) })
      await page.goto("/")
      await page.getByRole("button", { name: "Actions for api-server", exact: true }).click()
      await page.getByRole("menuitem", { name: "Edit startup command", exact: true }).click()
      await expect(page.getByRole("textbox", { name: "Startup command", exact: true })).toHaveValue("sh")
      await page.getByRole("textbox", { name: "Startup command", exact: true }).fill("sh && echo bad")
      await page.getByRole("button", { name: "Save and restart" }).click()
      await expect(page.getByRole("dialog").getByRole("alert")).toContainText("one executable")
      expect(replacements).toBe(0)
      await page.getByRole("textbox", { name: "Startup command", exact: true }).fill("sh")
      await page.getByRole("button", { name: "Save and restart" }).click()
      await expect(page.getByRole("dialog").getByRole("alert")).toContainText("startup management denied")
      expect(replacements).toBe(1)
    })
  })
}

// Helpers — machine entries in workspace settings are <button>s whose accessible
// name includes the machine name, root path and build label.
const workstationRow = (page: Page) => page.getByRole("button", { name: /^workstation / })
const cloudboxRow = (page: Page) => page.getByRole("button", { name: /^cloudbox / })
const agentsTab = (page: Page) => page.locator("aside").getByText("Tree", { exact: true })
const appsTab = agentsTab

async function openWorkspaceSettings(page: Page) {
  await page.getByRole("button", { name: "Workspace settings" }).click()
  await expect(page.getByRole("heading", { name: "Launch profiles" })).toBeVisible()
}

async function expandFolder(page: Page, name: string) {
  const row = page.getByRole("treeitem", { name, exact: true })
  await expect(row).toBeVisible()
  if (await row.getAttribute("aria-expanded") !== "true") await row.getByRole("button", { name, exact: true }).click()
}

async function createResearchTree(page: Page) {
  await page.goto("/")
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add folder", exact: true }).click()
  await page.getByRole("textbox", { name: "Folder name" }).fill("research")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await page.getByRole("button", { name: "Actions for research", exact: true }).click()
  await page.getByRole("menuitem", { name: "New folder", exact: true }).click()
  await page.getByRole("textbox", { name: "Folder name" }).fill("experiments")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expandFolder(page, "research")
  await expandFolder(page, "experiments")
  await expect(page.getByRole("treeitem", { name: "experiments", exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Actions for api-server", exact: true }).click()
  await page.getByRole("menuitem", { name: "Move to folder", exact: true }).click()
  await page.getByRole("combobox", { name: "Destination folder" }).selectOption("folder-2")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page.getByRole("treeitem", { name: "experiments", exact: true }).getByRole("treeitem", { name: "api-server", exact: true })).toBeVisible()
}

for (const mobile of [false, true]) {
  test.describe(`tree menu on ${mobile ? "mobile" : "desktop"}`, () => {
    test.use({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 }, hasTouch: mobile, isMobile: mobile })

    for (const launch of ["Terminal", "Custom command", "Codex"]) {
      test(`New Agent reuses ${launch} creation and appends to the chosen folder`, async ({ page }) => {
        await createResearchTree(page)
        const tree = await page.evaluate(async () => (await fetch("/api/workspaces/ws-1/workload-tree")).json())
        await page.route("**/api/workspaces/ws-1/workload-tree", (route) => ok(route, tree))
        let created = false
        let creations = 0
        const reviewer = { ...agentA, agent_id: "ag-reviewer", name: "reviewer" }
        await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, { ...snapshot, agents: created ? [...snapshot.agents, reviewer] : snapshot.agents }))
        const endpoint = launch === "Codex" ? "/launch-profiles/alp-codex/launch" : "/agents"
        await page.route(`**/api/workspaces/ws-1${endpoint}`, (route) => {
          const body = route.request().postDataJSON()
          expect(body.server_id).toBe("srv-a")
          expect(body.folder_id).toBe("folder-2")
          expect(launch === "Codex" ? body.agent_name : body.name).toBe("reviewer")
          if (launch === "Custom command") expect(body.args).toEqual(["echo", "hello world"])
          if (launch === "Terminal") expect(body.kind).toBe("shell")
          creations++; created = true
          tree.placements.push({ agent_id: reviewer.agent_id, folder_id: body.folder_id, position: 1 })
          return ok(route, reviewer)
        })
        await page.getByRole("button", { name: "Actions for experiments", exact: true }).click()
        await page.getByRole("menuitem", { name: "New Agent", exact: true }).click()
        const dialog = page.getByRole("dialog")
        await expect(dialog.getByRole("heading", { name: "Create agent" })).toBeVisible()
        await expect(dialog).toContainText("Tree folder: /research/experiments")
        if (launch !== "Terminal") {
          await dialog.getByRole("combobox", { name: "Launch", exact: true }).click()
          await page.getByRole("option", { name: launch, exact: true }).click()
        }
        if (launch === "Custom command") await dialog.getByRole("textbox", { name: "Command", exact: true }).fill('echo "hello world"')
        await dialog.getByRole("textbox", { name: "Name", exact: true }).fill("reviewer")
        const placementWrites: string[] = []
        page.on("request", (request) => { if (request.method() === "PUT" && request.url().includes("/placements/ag-reviewer")) placementWrites.push(request.url()) })
        await dialog.getByRole("button", { name: launch === "Terminal" ? "Create terminal" : "Create agent", exact: true }).click()
        await expect.poll(() => creations).toBe(1)
        expect(placementWrites).toEqual([])
        if (mobile) await page.getByRole("button", { name: /^Close full-screen (terminal|interface)$/ }).click()
        await expect(page.getByRole("treeitem", { name: "experiments", exact: true }).getByRole("treeitem", { name: "reviewer", exact: true })).toBeVisible()
        await page.reload()
        await expect(page.getByRole("treeitem", { name: "experiments", exact: true }).getByRole("treeitem", { name: "reviewer", exact: true })).toBeVisible()
        expect(creations).toBe(1)
      })
    }

    test("Copy path keeps exact CLI folder and Agent paths", async ({ page }) => {
      await page.addInitScript(() => {
        const saved = window as Window & { copiedPaths: string[] }
        saved.copiedPaths = []
        Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async (text: string) => { saved.copiedPaths.push(text) } } })
      })
      await createResearchTree(page)
      for (const [name, path] of [["research", "/research"], ["experiments", "/research/experiments"], ["api-server", "/research/experiments/api-server"], ["worker", "/worker"], ["Soul Archive", "/Soul Archive"]]) {
        await page.getByRole("button", { name: `Actions for ${name}`, exact: true }).click()
        await page.getByRole("menuitem", { name: "Copy path", exact: true }).click()
        await expect(page.getByRole("status").filter({ hasText: `Copied path: ${path}` })).toBeVisible()
        await expect.poll(() => page.evaluate(() => (window as Window & { copiedPaths: string[] }).copiedPaths.at(-1))).toBe(path)
      }
      await page.getByRole("button", { name: "Actions for experiments", exact: true }).click()
      await page.getByRole("menuitem", { name: "Rename", exact: true }).click()
      await page.getByRole("textbox", { name: "Folder name" }).fill("Notes & 一")
      await page.getByRole("button", { name: "Save", exact: true }).click()
      await page.getByRole("button", { name: "Actions for api-server", exact: true }).click()
      await page.getByRole("menuitem", { name: "Copy path", exact: true }).click()
      await expect.poll(() => page.evaluate(() => (window as Window & { copiedPaths: string[] }).copiedPaths.at(-1))).toBe("/research/Notes & 一/api-server")
    })

    for (const clipboard of ["missing", "denied"]) {
      test(`Copy path provides selected text when clipboard is ${clipboard}`, async ({ page }) => {
        await page.addInitScript((mode) => Object.defineProperty(navigator, "clipboard", {
          configurable: true, value: mode === "missing" ? undefined : { writeText: async () => { throw new Error("Permission denied") } },
        }), clipboard)
        await createResearchTree(page)
        for (const [name, path] of [["experiments", "/research/experiments"], ["api-server", "/research/experiments/api-server"]]) {
          await page.getByRole("button", { name: `Actions for ${name}`, exact: true }).click()
          await page.getByRole("menuitem", { name: "Copy path", exact: true }).click()
          const dialog = page.getByRole("dialog")
          await expect(dialog).toContainText("Automatic copying is unavailable")
          const input = dialog.getByRole("textbox", { name: "Tree path" })
          await expect(input).toHaveValue(path)
          await expect(input).toBeFocused()
          expect(await input.evaluate((element: HTMLInputElement) => element.selectionEnd! - element.selectionStart!)).toBe(path.length)
          await dialog.getByRole("button", { name: "Done" }).click()
          await expect(page.getByRole("status").filter({ hasText: "Unable to copy automatically" })).toBeVisible()
        }
      })
    }

    test("folder conflicts reject creation atomically and root New forgets the folder", async ({ page }) => {
      await createResearchTree(page)
      const created = false
      let creations = 0
      const reviewer = { ...agentA, agent_id: "ag-reviewer", name: "reviewer" }
      await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, { ...snapshot, agents: created ? [...snapshot.agents, reviewer] : snapshot.agents }))
      await page.route("**/api/workspaces/ws-1/agents", (route) => { creations++; expect(route.request().postDataJSON().folder_id).toBe("folder-2"); return route.fulfill({ status: 409, contentType: "application/json", body: JSON.stringify({ error: { code:"agent_name_conflict", message: "An Agent in this folder already has this name" } }) }) })
      await page.route("**/api/workspaces/ws-1/workload-tree/placements/ag-reviewer", (route) => route.fulfill({ status: 404, contentType: "application/json", body: JSON.stringify({ error: { message: "Folder no longer exists" } }) }))
      await page.getByRole("button", { name: "Actions for experiments", exact: true }).click()
      await page.getByRole("menuitem", { name: "New Agent", exact: true }).click()
      await page.getByRole("dialog").getByRole("textbox", { name: "Name", exact: true }).fill("reviewer")
      await page.getByRole("dialog").getByRole("button", { name: "Create terminal", exact: true }).click()
      await expect(page.getByText("An Agent in this folder already has this name", { exact:true })).toBeVisible()
      await expect(page.getByRole("dialog").getByRole("textbox", { name: "Name", exact: true })).toHaveValue("reviewer")
      await page.getByRole("dialog").getByRole("button", { name: "Cancel", exact: true }).click()
      await expect(page.getByRole("treeitem", { name: "reviewer", exact: true })).toHaveCount(0)
      await expect(page.getByRole("dialog")).toHaveCount(0)
      expect(creations).toBe(1)
      await page.getByRole("button", { name: "Add to tree", exact: true }).click()
      await page.getByRole("menuitem", { name: "Add agent", exact: true }).click()
      await expect(page.getByRole("dialog")).not.toContainText("Tree folder:")
    })
  })
}

test("opens app, shows org, workspace, Apps and agents", async ({ page }) => {
  await page.goto("/")
  await expect(page.locator("aside").getByText("Acme")).toBeVisible()
  await expect(page.getByRole("combobox", { name: "Workspace" })).toHaveText("Demo")
  await expect.poll(() => new URL(page.url()).pathname).toBe("/orgs/org-1/workspaces/ws-1")
  await expect(page).toHaveTitle("Acme / Demo")
  await expect(page.getByRole("tree", { name: "Workspace workloads" })).toBeVisible()
  await expect(page.locator("aside [role=tab]")).toHaveCount(0)
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toHaveCount(1)
  await expect(page.getByRole("button", { name: /^api-server / })).toBeVisible()
  await expect(page.getByRole("button", { name: /^worker / })).toBeVisible()
  await expect(page.getByRole("button", { name: /^app:Soul Archive / })).toHaveCount(0)
})

test("mixed workloads use common lifecycle endpoints and logs", async ({ page }) => {
  await page.goto("/")
  for (const [name, id] of [["api-server", "ag-1"], ["Soul Archive", "appw-1"]]) {
    for (const action of ["Start", "Stop", "Restart"]) {
      await page.getByRole("button", { name: `Actions for ${name}`, exact: true }).click()
      const request = page.waitForRequest((request) => request.url().endsWith(`/agents/${id}/${action.toLowerCase()}`) && request.method() === "POST")
      await page.getByRole("menuitem", { name: action, exact: true }).click()
      await request
    }
    await page.getByRole("button", { name: `Actions for ${name}`, exact: true }).click()
    await page.getByRole("menuitem", { name: "Logs", exact: true }).click()
    await expect(page.getByRole("treeitem", { name, exact: true })).toHaveAttribute("aria-selected", "true")
  }
})

test("Tree and map distinguish App identity from a command runtime kind", async ({ page }) => {
  await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, { ...snapshot, agents: [agentA, agentB, { ...appRuntimeAgent, kind: "command" }] }))
  await page.goto("/")
  await expect(page.getByRole("button", { name: /^api-server agent ·/ })).toBeVisible()
  await expect(page.getByRole("button", { name: /^worker agent ·/ })).toBeVisible()
  await expect(page.getByRole("button", { name: /^Soul Archive app ·/ })).toBeVisible()
  await page.getByRole("button", { name: "Expand tree map" }).click()
  const app = page.getByRole("dialog").getByRole("button", { name: /^Open \/Soul Archive · app ·/ })
  await expect(app).toBeVisible()
  await expect(app).toHaveAttribute("title", / · app ·/)
  await expect(page.getByRole("dialog").getByRole("button", { name: /^Open \/api-server · agent ·/ })).toBeVisible()
})

test("folder loading failure keeps workloads usable and supports retry", async ({ page }) => {
  let fail = true
  await page.route("**/api/workspaces/ws-1/workload-tree", async (route) => {
    if (fail) return route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: "Temporarily unavailable" }) })
    return ok(route, { folders: [{ folder_id: "restored", name: "Recovered folder", parent_id: null }], placements: [] })
  })
  await page.goto("/")
  await expect(page.getByRole("alert")).toBeVisible()
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toBeVisible()
  fail = false
  await page.getByRole("button", { name: "Reload folders", exact: true }).click()
  await expect(page.getByRole("treeitem", { name: "Recovered folder", exact: true })).toBeVisible()
  await expect(page.getByRole("alert")).toHaveCount(0)
})

test("external folder changes refresh without resetting an open editor", async ({ page }) => {
  await page.clock.install()
  let folders: { folder_id: string; name: string; parent_id: string | null }[] = []
  await page.route("**/api/workspaces/ws-1/workload-tree", (route) => ok(route, { folders, placements: folders.length ? [{ agent_id: "ag-1", folder_id: "cli-folder", position: 0 }] : [] }))
  await page.goto("/")
  await expect(page.getByRole("treeitem", { name: "api-server", exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add folder", exact: true }).click()
  await page.getByRole("textbox", { name: "Folder name" }).fill("Unsaved folder")
  folders = [{ folder_id: "cli-folder", name: "From CLI", parent_id: null }]
  await page.clock.fastForward(5100)
  await expect(page.getByRole("treeitem", { name: "From CLI", exact: true, includeHidden: true })).toBeAttached()
  await expect(page.getByRole("textbox", { name: "Folder name" })).toHaveValue("Unsaved folder")
  await page.getByRole("button", { name: "Cancel", exact: true }).click()
  await expandFolder(page, "From CLI")
  await expect(page.getByRole("treeitem", { name: "From CLI", exact: true }).getByRole("treeitem", { name: "api-server", exact: true })).toBeVisible()
})

test("a stale background tree read cannot undo a local folder mutation", async ({ page }) => {
  let blockRead = false
  let release: (() => void) | undefined
  let started: (() => void) | undefined
  const pending = new Promise<void>((resolve) => { started = resolve })
  await page.route("**/api/workspaces/ws-1/workload-tree", async (route) => {
    if (!blockRead) return route.fallback()
    blockRead = false
    started!()
    await new Promise<void>((resolve) => { release = resolve })
    return ok(route, { folders: [], placements: [] })
  })
  await page.goto("/")
  await expect(page.getByRole("button", { name: "Add to tree", exact: true })).toBeEnabled()
  blockRead = true
  await page.evaluate(() => window.dispatchEvent(new Event("focus")))
  await pending
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add folder", exact: true }).click()
  await page.getByRole("textbox", { name: "Folder name" }).fill("Keep this folder")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(page.getByRole("treeitem", { name: "Keep this folder", exact: true })).toBeVisible()
  const staleRead = page.waitForResponse((response) => response.url().endsWith("/workload-tree"))
  release!()
  await staleRead
  await expect(page.getByRole("treeitem", { name: "Keep this folder", exact: true })).toBeVisible()
})

test("an App missing from machine inventory remains visible with its stable runtime ID", async ({ page }) => {
  await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, { ...snapshot, agents: [agentA, agentB] }))
  await page.goto("/")
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toHaveCount(1)
  await page.getByRole("button", { name: "Actions for Soul Archive", exact: true }).click()
  await page.getByRole("menuitem", { name: "Logs", exact: true }).click()
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toHaveAttribute("aria-selected", "true")
  await expect(page.locator("header").getByText("Soul Archive", { exact: true })).toBeVisible()
})

test("shared App rename and delete update metadata without resurrecting a placeholder", async ({ page }) => {
  let runtime = { ...appRuntimeAgent }
  let app = { ...managedApp }
  let deleted = false
  await page.route("**/api/workspaces/ws-1/apps", (route) => ok(route, { apps: deleted ? [] : [app] }))
  await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, { ...snapshot, agents: [agentA, agentB, ...(deleted ? [] : [runtime])] }))
  await page.route("**/api/workspaces/ws-1/agents/appw-1", async (route) => {
    if (route.request().method() === "PATCH") {
      const name = route.request().postDataJSON().name
      runtime = { ...runtime, name }
      app = { ...app, name }
    }
    if (route.request().method() === "DELETE") deleted = true
    return ok(route, runtime)
  })
  await page.goto("/")
  await page.getByRole("button", { name: "Actions for Soul Archive", exact: true }).click()
  await page.getByRole("menuitem", { name: "Rename", exact: true }).click()
  await page.getByRole("dialog").getByRole("textbox").fill("Research Archive")
  await page.getByRole("dialog").getByRole("button", { name: "Rename", exact: true }).click()
  await expect(page.getByRole("treeitem", { name: "Research Archive", exact: true })).toBeVisible()
  const renamedList = page.waitForResponse((response) => response.url().endsWith("/ws-1/apps"))
  await page.evaluate(() => dispatchEvent(new Event("focus")))
  expect((await renamedList).ok()).toBe(true)
  await expect(page.getByRole("treeitem", { name: "Research Archive", exact: true })).toBeVisible()
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toHaveCount(0)
  await page.getByRole("button", { name: "Actions for Research Archive", exact: true }).click()
  await page.getByRole("menuitem", { name: "App settings", exact: true }).click()
  await expect(page.getByRole("heading", { name: "Research Archive", exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Close", exact: true }).click()
  await page.getByRole("button", { name: "Actions for Research Archive", exact: true }).click()
  await page.getByRole("menuitem", { name: "Delete", exact: true }).click()
  await page.getByRole("dialog").getByRole("button", { name: "Delete", exact: true }).click()
  await expect.poll(() => deleted).toBe(true)
  const deletedList = page.waitForResponse((response) => response.url().endsWith("/ws-1/apps"))
  await page.evaluate(() => dispatchEvent(new Event("focus")))
  expect(await (await deletedList).json()).toEqual({ apps: [] })
  await expect(page.getByRole("treeitem", { name: "Research Archive", exact: true })).toHaveCount(0)
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toHaveCount(0)
  await expect(page.getByRole("treeitem")).toHaveCount(2)
})

test("remote App rename and delete events keep tree metadata synchronized", async ({ page }) => {
  let remoteApps = [managedApp]
  await page.route('**/api/workspaces/ws-1/apps', (route) => ok(route, { apps: remoteApps }))
  // The mock server sees a connection before the browser's OPEN event. Wait
  // for that event rather than sending into a connecting/StrictMode socket.
  await page.addInitScript(() => {
    const NativeSocket = window.WebSocket
    window.WebSocket = class extends NativeSocket {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols)
        if (String(url).endsWith("/events")) {
          const saved = window as Window & { workspaceEvents?: WebSocket }
          saved.workspaceEvents = this
        }
      }
    }
  })
  let events: { send: (message: string) => void } | undefined
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/events$/, (socket) => { events = socket })
  await page.goto("/")
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toBeVisible()
  await expect.poll(() => Boolean(events)).toBe(true)
  await expect.poll(() => page.evaluate(() => (window as Window & { workspaceEvents?: WebSocket }).workspaceEvents?.readyState)).toBe(1)
  remoteApps = [{ ...managedApp, name: 'Remote Archive' }]
  events!.send(JSON.stringify({ event: "agent.updated", revision: 2, data: { ...appRuntimeAgent, name: "Remote Archive" } }))
  await expect(page.getByRole("treeitem", { name: "Remote Archive", exact: true })).toBeVisible()
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toHaveCount(0)
  remoteApps = []
  events!.send(JSON.stringify({ event: "agent.deleted", revision: 3, data: appRuntimeAgent }))
  await expect(page.getByRole("treeitem", { name: "Remote Archive", exact: true })).toHaveCount(0)
  await expect(page.getByRole("treeitem")).toHaveCount(2)
})

test("workloads start in name order independent of machine inventory order", async ({ page }) => {
  await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, { ...snapshot, agents: [...snapshot.agents].reverse() }))
  await page.goto("/")
  await expect(page.getByRole("treeitem")).toHaveCount(3)
  await expect.poll(() => page.getByRole("treeitem").evaluateAll((rows) => rows.map((row) => row.getAttribute("aria-label")))).toEqual(["api-server", "Soul Archive", "worker"])
})

test("dragging workloads before and after siblings saves order across reloads", async ({ page }) => {
  await page.goto("/")
  const row = (name: string) => page.getByRole("treeitem", { name, exact: true })
  const names = () => page.getByRole("treeitem").evaluateAll((rows) => rows.map((item) => item.getAttribute("aria-label")))
  await expect.poll(names).toEqual(["api-server", "Soul Archive", "worker"])
  await row("worker").dragTo(row("api-server"), { targetPosition: { x: 20, y: 4 } })
  await expect.poll(names).toEqual(["worker", "api-server", "Soul Archive"])
  await page.reload()
  await expect.poll(names).toEqual(["worker", "api-server", "Soul Archive"])
  const bottom = (await row("Soul Archive").boundingBox())!.height - 4
  await row("worker").dragTo(row("Soul Archive"), { targetPosition: { x: 20, y: bottom } })
  await expect.poll(names).toEqual(["api-server", "Soul Archive", "worker"])
  await page.reload()
  await expect.poll(names).toEqual(["api-server", "Soul Archive", "worker"])
})

test("mobile workload menus move entries up and down and disable list boundaries", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  const names = () => page.getByRole("treeitem").evaluateAll((rows) => rows.map((item) => item.getAttribute("aria-label")))
  await expect.poll(names).toEqual(["api-server", "Soul Archive", "worker"])
  await page.getByRole("button", { name: "Actions for api-server", exact: true }).click()
  await expect(page.getByRole("menuitem", { name: "Move up", exact: true })).toHaveAttribute("aria-disabled", "true")
  await page.getByRole("menuitem", { name: "Move down", exact: true }).click()
  await expect.poll(names).toEqual(["Soul Archive", "api-server", "worker"])
  await page.getByRole("button", { name: "Actions for worker", exact: true }).click()
  await expect(page.getByRole("menuitem", { name: "Move down", exact: true })).toHaveAttribute("aria-disabled", "true")
  await page.getByRole("menuitem", { name: "Move up", exact: true }).click()
  await expect.poll(names).toEqual(["Soul Archive", "worker", "api-server"])
  await page.reload()
  await expect.poll(names).toEqual(["Soul Archive", "worker", "api-server"])
})

test("dragging into a folder inserts at the chosen sibling and preserves that order", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add folder", exact: true }).click()
  await page.getByRole("textbox", { name: "Folder name" }).fill("Research")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  const folder = page.getByRole("treeitem", { name: "Research", exact: true })
  await expandFolder(page, "Research")
  for (const name of ["worker", "Soul Archive"]) {
    await page.getByRole("button", { name: `Actions for ${name}`, exact: true }).click()
    await page.getByRole("menuitem", { name: "Move to folder", exact: true }).click()
    await page.getByRole("combobox", { name: "Destination folder" }).selectOption("folder-1")
    await page.getByRole("button", { name: "Save", exact: true }).click()
    await expect(folder.getByRole("treeitem", { name, exact: true })).toBeVisible()
  }
  await page.getByRole("treeitem", { name: "api-server", exact: true }).dragTo(folder.getByRole("treeitem", { name: "Soul Archive", exact: true }), { targetPosition: { x: 20, y: 4 } })
  const names = () => folder.getByRole("treeitem").evaluateAll((rows) => rows.map((item) => item.getAttribute("aria-label")))
  await expect.poll(names).toEqual(["worker", "api-server", "Soul Archive"])
  await page.reload()
  await expect.poll(names).toEqual(["worker", "api-server", "Soul Archive"])
})

test("folders support persistent drag and keyboard moves, nesting, rename and deletion", async ({ page }) => {
  await page.goto("/")
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add folder", exact: true }).click()
  await page.getByRole("textbox", { name: "Folder name" }).fill("Research")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  const folder = page.getByRole("treeitem", { name: "Research", exact: true })
  await expandFolder(page, "Research")
  await expect(folder).toBeVisible()
  const moved = page.waitForRequest((request) => request.url().endsWith("/workload-tree/placements/ag-1") && request.method() === "PUT")
  await page.getByRole("treeitem", { name: "api-server", exact: true }).dragTo(folder.locator(":scope > div").first())
  expect((await moved).postDataJSON()).toEqual({ folder_id: "folder-1", position: 0 })
  await expect(folder.getByRole("treeitem", { name: "api-server", exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Actions for Soul Archive", exact: true }).click()
  await page.getByRole("menuitem", { name: "Move to folder", exact: true }).click()
  await page.getByRole("combobox", { name: "Destination folder" }).selectOption("folder-1")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(folder.getByRole("treeitem", { name: "Soul Archive", exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Actions for Research", exact: true }).click()
  await page.getByRole("menuitem", { name: "New folder", exact: true }).click()
  await page.getByRole("textbox", { name: "Folder name" }).fill("Models")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(folder.getByRole("treeitem", { name: "Models", exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Actions for Research", exact: true }).click()
  await page.getByRole("menuitem", { name: "Move to folder", exact: true }).click()
  await expect(page.getByRole("combobox", { name: "Destination folder" }).getByRole("option")).toHaveCount(1)
  await page.getByRole("button", { name: "Cancel", exact: true }).click()
  await page.reload()
  await expect(folder.getByRole("treeitem", { name: "api-server", exact: true })).toBeVisible()
  await expect(folder.getByRole("treeitem", { name: "Soul Archive", exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Actions for Models", exact: true }).click()
  await page.getByRole("menuitem", { name: "Rename", exact: true }).click()
  await page.getByRole("textbox", { name: "Folder name" }).fill("Experiments")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(folder.getByRole("treeitem", { name: "Experiments", exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Actions for Experiments", exact: true }).click()
  await page.getByRole("menuitem", { name: "Move to folder", exact: true }).click()
  await page.getByRole("combobox", { name: "Destination folder" }).selectOption("")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await expect(folder.getByRole("treeitem", { name: "Experiments", exact: true })).toHaveCount(0)
  await expect(page.getByRole("treeitem", { name: "Experiments", exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Actions for Research", exact: true }).click()
  await page.getByRole("menuitem", { name: "Delete", exact: true }).click()
  await expect(page.getByRole("treeitem", { name: "Research", exact: true })).toHaveCount(0)
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toBeVisible()
  await expect(page.getByRole("treeitem", { name: "Experiments", exact: true })).toBeVisible()
})

test("workload folders remain workspace scoped and mobile navigation returns to the tree", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add folder", exact: true }).click()
  await page.getByRole("textbox", { name: "Folder name" }).fill("Private organization")
  await page.getByRole("button", { name: "Save", exact: true }).click()
  await page.getByRole("combobox", { name: "Organization" }).click()
  await page.getByRole("option", { name: "Research", exact: true }).click()
  await expect(page.getByRole("treeitem", { name: "Private organization", exact: true })).toHaveCount(0)
  await page.getByRole("combobox", { name: "Organization" }).click()
  await page.getByRole("option", { name: "Acme", exact: true }).click()
  await expect(page.getByRole("treeitem", { name: "Private organization", exact: true })).toBeVisible()
  await page.getByRole("treeitem", { name: "Soul Archive", exact: true }).getByRole("button", { name: /^Soul Archive / }).click()
  await expect(page.getByRole("button", { name: "Close full-screen interface", exact: true })).toBeVisible()
  await page.getByRole("button", { name: "Close full-screen interface", exact: true }).click()
  await expect(page.getByRole("tree", { name: "Workspace workloads" })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
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
  await expect(page.getByRole("treeitem")).toHaveCount(0)
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

for (const mobile of [false, true]) test(`new agent visibly guides an empty workspace to machine setup (${mobile ? '390px' : 'desktop'})`, async ({ page }) => {
  if (mobile) await page.setViewportSize({ width: 390, height: 844 })
  await page.route("**/api/workspaces/ws-1/bootstrap", (route) => ok(route, { install_command: "echo synthetic install instructions", connect_command: "echo synthetic connect instructions" }))
  await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, { ...snapshot, servers: [], agents: [] }))
  await page.goto("/")
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await expect(page.getByText("Set up a machine first", { exact: true })).toBeVisible()
  const addMachinePrompt = page.getByRole("menuitem", { name: "Set up a machine before creating an agent" })
  await expect(addMachinePrompt).toBeEnabled()
  await addMachinePrompt.click()
  await expect(page.getByText("Add a machine to create agents", { exact: true })).toBeVisible()
  await expect(page.getByText("Agents run on machines.", { exact: false })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Create agent" })).toHaveCount(0)
  await page.getByRole("button", { name: "Set up a machine", exact: true }).click()
  await expect(page.getByRole("dialog").getByRole("heading", { name: "Add machine", exact: true })).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
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

test("workspace settings show the default policy and install its bundled App", async ({ page }) => {
  await page.goto("/")
  await openWorkspaceSettings(page)

  await expect(page.getByText("Treer Default", { exact: true })).toBeVisible()
  await expect(page.getByText("Active · monitor · allow", { exact: true })).toBeVisible()
  await expect(page.getByRole("combobox", { name: "Default Policy App machine" })).toHaveText("workstation")

  const installRequest = page.waitForRequest((request) => request.url().endsWith("/api/workspaces/ws-1/policy-provider/default-app"))
  await page.getByRole("button", { name: "Install Default Policy App" }).click()
  expect((await installRequest).postDataJSON()).toEqual({ server_id: "srv-a" })
  await expect(page.getByRole("combobox", { name: "Policy App", exact: true })).toHaveText("Treer Policy")
  await expect(page.getByRole("link", { name: "Open Policy App" })).toBeVisible()
})

test("default policy controls fit a mobile workspace view", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await openWorkspaceSettings(page)
  await expect(page.getByRole("button", { name: "Install Default Policy App" })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
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
  await page.getByRole("button", { name: "Actions for Soul Archive" }).click()
  await page.getByRole("menuitem", { name: "App settings", exact: true }).click()
  await expect(page.getByRole("heading", { name: "Network" })).toHaveCount(0)
  await expect(page.getByText(/_human/)).toHaveCount(0)
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
  const restarting = page.waitForRequest((request) => request.url().includes("/agents/appw-1/restart") && request.method() === "POST")
  await page.getByRole("button", { name: "Restart" }).click()
  await restarting
})

test("mobile App settings hide the sidebar and fit the viewport", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await page.getByRole("button", { name: "Actions for Soul Archive" }).click()
  await page.getByRole("menuitem", { name: "App settings", exact: true }).click()

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

test("agent toolbar uploads a file to its machine", async ({ page }) => {
  let requestBody: unknown
  await page.route("**/api/workspaces/ws-1/machines/srv-a/files", async (route) => {
    requestBody = route.request().postDataJSON()
    await ok(route, {
      server_id: "srv-a",
      path: "repo/hello.txt",
      bytes_written: 5,
    })
  })

  await page.goto("/")
  await page.getByRole("button", { name: /^api-server / }).click()
  await page.getByRole("button", { name: "Upload file" }).last().click()
  const dialog = page.getByRole("dialog")
  await expect(dialog.getByRole("heading", { name: "Upload file" })).toBeVisible()
  await expect(dialog.getByText("Upload to workstation.")).toBeVisible()
  await dialog.getByLabel("Destination directory").fill("repo")
  await dialog.locator('input[type="file"]').setInputFiles({
    name: "hello.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("hello"),
  })
  await dialog.getByRole("checkbox").check()
  await dialog.getByRole("button", { name: "Upload", exact: true }).click()

  await expect(dialog.getByText("Uploaded repo/hello.txt (5 B).")).toBeVisible()
  expect(requestBody).toEqual({
    directory: "repo",
    file_name: "hello.txt",
    content_base64: "aGVsbG8=",
    overwrite: true,
  })
})

test("mobile upload dialog fits the viewport", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await page.getByRole("button", { name: /^api-server / }).click()
  await page.getByRole("button", { name: "Upload file" }).last().click()

  await expect(page.getByRole("dialog").getByRole("heading", { name: "Upload file" })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
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
  await expect(page.getByRole("button", { name: "Copy restore" })).toBeVisible()
})

test("machine overview only shows agents for the selected machine", async ({ page }) => {
  await page.goto("/")
  await openWorkspaceSettings(page)
  await workstationRow(page).click()
  await expect(page.getByRole("heading", { name: "workstation" })).toBeVisible()

  // Only the agent on srv-a should appear in the overview agent list
  const agentsSection = page.locator("main section", { has: page.getByRole("heading", { name: "Agents on this machine" }) })
  await expect(agentsSection.getByRole("button", { name: "Terminal", exact: true })).toHaveCount(2)
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
  await page.getByRole("button", { name: "Terminal", exact: true }).first().click()

  // Returns to the terminal main view; header shows agent name
  await expect(page.locator("header").getByText("api-server")).toBeVisible()
})

test("terminal drains a burst of small output frames without stalling", async ({ page }) => {
  const frameCount = 800
  let outputBytes = 0
  let acknowledgedBytes = 0
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/agents\/[^/]+\/terminal(?:\?.*)?$/, (ws) => {
    ws.onMessage((message) => {
      if (typeof message !== "string") return
      const control = JSON.parse(message) as { type?: string; bytes?: number }
      if (control.type === "ack") acknowledgedBytes += control.bytes ?? 0
    })
    ws.send(JSON.stringify({
      type: "ready",
      session_id: "term-burst",
      stream_epoch: "epoch-burst",
      revision: 0,
      gap: false,
      replay_chunks: 0,
    }))
    for (let revision = 1; revision <= frameCount; revision += 1) {
      const output = Buffer.from(`burst-frame-${revision}\r\n`)
      outputBytes += output.byteLength
      ws.send(output)
      ws.send(JSON.stringify({ type: "cursor", stream_epoch: "epoch-burst", revision }))
    }
  })

  await page.goto("/")
  await page.getByRole("button", { name: /^api-server / }).click()

  await expect(page.locator(".xterm-rows")).toContainText(`burst-frame-${frameCount}`, { timeout: 2_000 })
  await expect.poll(() => acknowledgedBytes).toBe(outputBytes)
})

for (const mobile of [false, true]) {
  test.describe(`terminal replay queries on ${mobile ? '390px' : 'desktop'}`, () => {
    test.use({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 } })
    const queries = '\x1b]10;?\x07\x1b]11;?\x1b\\\x1b[c\x1b[>c\x1b[6n\x1b[?6n\x1b[5n\x1b]12;?\x07\x1b]4;1;?\x07\x1b[4$p\x1b[?25$p\x1bP$qm\x1b\\\x1b[?15n\x1b[?25n\x1b[?26n\x1b[?53n'
    async function open(page: Page) {
      await page.goto('/')
      await page.getByRole('button', { name: /^api-server / }).click()
      await expect(page.locator('.xterm')).toBeVisible()
    }
    test('split replay is silent, queued live queries respond, and typed input is unchanged', async ({ page }) => {
      const input: string[] = []
      let ack = 0
      const history = ['history\r\n\x1b]10;', '?\x07' + queries + '\r\nreplay-end\r\n']
      const live = queries + '\r\nlive-end\r\n'
      await page.routeWebSocket(/\/agents\/[^/]+\/terminal(?:\?.*)?$/, (ws) => {
        ws.onMessage((message) => {
          if (typeof message !== 'string') input.push(Buffer.from(message).toString())
          else { const control = JSON.parse(message); if (control.type === 'ack') ack += control.bytes }
        })
        ws.send(JSON.stringify({ type: 'ready', stream_epoch: 'queries', revision: 2, replay_chunks: history.length }))
        history.forEach((chunk) => ws.send(Buffer.from(chunk)))
        // Queue live output before the asynchronous replay write callbacks run.
        ws.send(Buffer.from(live))
        ws.send(JSON.stringify({ type: 'cursor', stream_epoch: 'queries', revision: 3 }))
      })
      await open(page)
      await expect.poll(() => ack).toBe(Buffer.byteLength(history.join('') + live))
      await expect(page.locator('.xterm-rows')).toContainText('live-end')
      expect(input).toHaveLength(12) // One per supported live query; private DSR no-ops stay silent.
      expect(input.join('')).toContain('10;rgb:d8d8/dcdc/dfdf')
      expect(input.join('')).toContain('11;rgb:0f0f/1212/1515')
      expect(input.join('')).toMatch(/\x1b\[\?[\d;]+c/)
      expect(input.join('')).toMatch(/\x1b\[>[\d;]+c/)
      expect(input.join('')).toMatch(/\x1b\[\d+;\d+R/)
      input.length = 0
      await page.locator('.xterm-helper-textarea').pressSequentially('hello')
      await page.locator('.xterm-helper-textarea').press('Enter')
      await expect.poll(() => input.join('')).toBe('hello\r')
    })
    test('user keys during replay survive; reconnect and refresh never answer historical queries', async ({ page }) => {
      const input: string[] = []
      let connections = 0
      let ack = 0
      let finishReplay: (() => void) | undefined
      let disconnect: (() => void) | undefined
      const first = 'partial\r\n' + queries
      const last = queries + '\r\nreplay-end\r\n'
      await page.routeWebSocket(/\/agents\/[^/]+\/terminal(?:\?.*)?$/, (ws) => {
        connections += 1
        ws.onMessage((message) => {
          if (typeof message !== 'string') input.push(Buffer.from(message).toString())
          else { const control = JSON.parse(message); if (control.type === 'ack') ack += control.bytes }
        })
        ws.send(JSON.stringify({ type: 'ready', stream_epoch: 'queries', revision: 2, gap: connections > 1 && !mobile, replay_chunks: 2 }))
        ws.send(Buffer.from(first))
        finishReplay = () => ws.send(Buffer.from(last))
        disconnect = () => ws.close()
        if (connections > 1) finishReplay()
      })
      await open(page)
      await expect.poll(() => ack).toBe(Buffer.byteLength(first))
      expect(input).toEqual([])
      await page.locator('.xterm-helper-textarea').pressSequentially('during-replay')
      await expect.poll(() => input.join('')).toBe('during-replay')
      input.length = 0
      finishReplay!()
      await expect.poll(() => ack).toBe(Buffer.byteLength(first + last))
      expect(input).toEqual([])
      disconnect!()
      await expect.poll(() => connections).toBe(2)
      await expect.poll(() => ack).toBe(2 * Buffer.byteLength(first + last))
      expect(input).toEqual([])
      await page.reload()
      if (mobile) await page.getByRole('button', { name: /^api-server / }).click()
      await expect.poll(() => connections).toBe(3)
      await expect.poll(() => ack).toBe(3 * Buffer.byteLength(first + last))
      expect(input).toEqual([])
    })
  })
}

test("terminal uses a browser-safe close code when output backlog exceeds its limit", async ({ page }) => {
  const pageErrors: Error[] = []
  page.on("pageerror", (error) => pageErrors.push(error))
  await page.addInitScript(() => {
    const originalClose = WebSocket.prototype.close
    const closeCodes: number[] = []
    Object.defineProperty(window, "__terminalCloseCodes", { value: closeCodes })
    WebSocket.prototype.close = function close(code?: number, reason?: string) {
      if (code !== undefined) closeCodes.push(code)
      if (code === undefined) return originalClose.call(this)
      return originalClose.call(this, code, reason)
    }
  })
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/agents\/[^/]+\/terminal(?:\?.*)?$/, (ws) => {
    ws.send(JSON.stringify({
      type: "ready",
      session_id: "term-backlog",
      stream_epoch: "epoch-backlog",
      revision: 0,
      gap: false,
      replay_chunks: 0,
    }))
    for (let frame = 0; frame <= 1024; frame += 1) ws.send(Buffer.from("A"))
  })

  await page.goto("/")
  await page.getByRole("button", { name: /^api-server / }).click()

  await expect.poll(() => page.evaluate(() => (window as unknown as { __terminalCloseCodes: number[] }).__terminalCloseCodes)).toContain(4001)
  expect(pageErrors).toEqual([])
})

test("create agent dialog offers profiles and explicit commands without provider installation", async ({ page }) => {
  await page.goto("/")
  await agentsTab(page).click()
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add agent", exact: true }).click()
  await page.getByRole("dialog").getByRole("combobox", { name: "Launch" }).click()
  await expect(page.getByRole("option", { name: "Terminal", exact: true })).toBeVisible()
  await expect(page.getByRole("option", { name: "Custom command", exact: true })).toBeVisible()
  await expect(page.getByRole("option", { name: /^Install / })).toHaveCount(0)
  await page.getByRole("option", { name: "Codex", exact: true }).click()
  await expect(page.getByRole("button", { name: "Create agent" })).toBeVisible()
})

test("create agent custom command preserves argv without a provider installer", async ({ page }) => {
  await page.goto("/")
  await agentsTab(page).click()
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add agent", exact: true }).click()
  await page.getByRole("dialog").getByRole("combobox", { name: "Launch" }).click()
  await page.getByRole("option", { name: "Custom command", exact: true }).click()
  await page.getByRole("textbox", { name: "Command", exact: true }).fill('/opt/my-agent --label "two words"')
  const creation = page.waitForRequest((request) => request.method() === "POST" && new URL(request.url()).pathname === "/api/workspaces/ws-1/agents")
  await page.getByRole("button", { name: "Create agent", exact: true }).click()
  expect((await creation).postDataJSON()).toMatchObject({
    server_id: "srv-a",
    kind: "command",
    cwd: ".",
    args: ["/opt/my-agent", "--label", "two words"],
  })
  await expect(page.getByRole("dialog")).toBeHidden()
})

test("create agent dialog lists an installed recipe launch profile", async ({ page }) => {
  await page.goto("/")
  await agentsTab(page).click()
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add agent", exact: true }).click()
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
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add agent", exact: true }).click()
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

for (const mobile of [false, true]) {
  test.describe(`same-name Agents on ${mobile ? "mobile" : "desktop"}`, () => {
    test.use({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 }, hasTouch: mobile, isMobile: mobile })
    test("selection, copy, rename and conflicting move keep stable identity", async ({ page }) => {
      const agents = [
        { ...agentA, name: "reviewer", path: "/research/reviewer" },
        { ...agentB, name: "reviewer", path: "/delivery/reviewer" },
      ]
      const tree = { folders: [
        { folder_id: "research", name: "research", parent_id: null },
        { folder_id: "delivery", name: "delivery", parent_id: null },
      ], placements: [
        { agent_id: "ag-1", folder_id: "research", position: 0 },
        { agent_id: "ag-2", folder_id: "delivery", position: 0 },
      ] }
      const mutations: string[] = []
      await page.addInitScript(() => {
        Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: async (path: string) => { (window as Window & { copiedPath: string }).copiedPath = path } } })
      })
      await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, { ...snapshot, agents }))
      await page.route("**/api/workspaces/ws-1/workload-tree**", (route) => {
        if (route.request().method() === "GET") return ok(route, tree)
        mutations.push(new URL(route.request().url()).pathname)
        expect(route.request().postDataJSON().folder_id).toBe("delivery")
        return route.fulfill({ status: 409, contentType: "application/json", body: JSON.stringify({ error: { code: "agent_name_conflict", message: "An Agent in this folder already has this name" } }) })
      })
      await page.route("**/api/workspaces/ws-1/agents/ag-1", (route) => {
        mutations.push(new URL(route.request().url()).pathname)
        const name = route.request().postDataJSON().name
        if (name === "occupied") return route.fulfill({ status: 409, contentType: "application/json", body: JSON.stringify({ error: { code: "agent_name_conflict", message: "An Agent in this folder already has this name" } }) })
        agents[0] = { ...agents[0], name, path: `/research/${name}` }
        return ok(route, agents[0])
      })
      await page.goto("/")
      await expandFolder(page, "research")
      await expandFolder(page, "delivery")
      const first = page.locator('[data-agent-id="ag-1"]')
      const second = page.locator('[data-agent-id="ag-2"]')
      await expect(first).toHaveAttribute("title", "/research/reviewer")
      await expect(second).toHaveAttribute("title", "/delivery/reviewer")
      for (const [row, path] of [[first, "/research/reviewer"], [second, "/delivery/reviewer"]] as const) {
        await row.getByRole("button").first().click()
        await expect(row).toHaveAttribute("aria-selected", "true")
        if (mobile) {
          await expect(page.locator(`span[title="${path}"]`)).toBeVisible()
          await page.getByRole("button", { name: "Close full-screen terminal", exact: true }).click()
        } else await expect(page.getByRole("navigation", { name: "Agent path" })).toHaveAttribute("title", path)
        await row.getByRole("button", { name: "Actions for reviewer", exact: true }).click()
        await page.getByRole("menuitem", { name: "Copy path", exact: true }).click()
        await expect.poll(() => page.evaluate(() => (window as Window & { copiedPath: string }).copiedPath)).toBe(path)
      }
      await first.getByRole("button", { name: "Actions for reviewer", exact: true }).click()
      await page.getByRole("menuitem", { name: "Move to folder", exact: true }).click()
      const dialog = page.getByRole("dialog")
      await dialog.getByRole("combobox", { name: "Destination folder" }).selectOption("delivery")
      await dialog.getByRole("button", { name: "Save", exact: true }).click()
      await expect(dialog.getByRole("alert")).toHaveText("An Agent in this folder already has this name")
      await expect(first).toHaveAttribute("title", "/research/reviewer")
      await dialog.getByRole("button", { name: "Cancel", exact: true }).click()
      await first.getByRole("button", { name: "Actions for reviewer", exact: true }).click()
      await page.getByRole("menuitem", { name: "Rename", exact: true }).click()
      await expect(dialog).toContainText("/research/reviewer")
      await dialog.getByRole("textbox").fill("occupied")
      await dialog.getByRole("button", { name: "Rename", exact: true }).click()
      await expect(dialog.getByRole("alert")).toHaveText("An Agent in this folder already has this name")
      await dialog.getByRole("textbox").fill("renamed")
      await dialog.getByRole("button", { name: "Rename", exact: true }).click()
      await expect(dialog).not.toBeVisible()
      await expect(first).toHaveAttribute("title", "/research/renamed")
      await expect(second).toHaveAttribute("title", "/delivery/reviewer")
      expect(mutations).toEqual([
        "/api/workspaces/ws-1/workload-tree/placements/ag-1",
        "/api/workspaces/ws-1/agents/ag-1", "/api/workspaces/ws-1/agents/ag-1",
      ])
    })
  })
}

test("startup command fits 390px and short screens with long argv and errors", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 390, height: 540 })
  const script = "echo 'long literal value'\n".repeat(90)
  await page.route("**/agents/ag-1/admin/startup", (route) => ok(route, { command: "sh", args: ["-lc", script], cwd: "repo" }))
  await page.route("**/agents/ag-1/restart-with-startup", (route) => route.fulfill({ status: 403, contentType: "application/json", body: JSON.stringify({ error: { message: "Denied: " + "long-path/".repeat(20) } }) }))
  await page.goto("/")
  await page.getByRole("button", { name: "Actions for api-server", exact: true }).click()
  await page.getByRole("menuitem", { name: "Edit startup command", exact: true }).click()
  const dialog = page.getByRole("dialog")
  const command = dialog.getByRole("textbox", { name: "Startup command", exact: true })
  await expect(command).toHaveValue(/long literal value/)
  await command.fill("sh -lc 'echo edited'")
  await dialog.getByRole("button", { name: "Save and restart" }).click()
  await expect(dialog.getByRole("alert")).toContainText("Denied:")
  await dialog.getByRole("button", { name: "Save and restart" }).scrollIntoViewIfNeeded()
  const box = (await dialog.boundingBox())!
  expect(box.x).toBeGreaterThanOrEqual(0)
  expect(box.x + box.width).toBeLessThanOrEqual(390)
  expect(box.y).toBeGreaterThanOrEqual(0)
  expect(box.y + box.height).toBeLessThanOrEqual(540)
  expect(await dialog.evaluate((node) => node.scrollWidth <= node.clientWidth)).toBe(true)
  await page.screenshot({ path: testInfo.outputPath("startup-390.png") })
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click()
  await page.setViewportSize({ width: 1280, height: 900 })
  await page.getByRole("button", { name: "Actions for api-server", exact: true }).click()
  await page.getByRole("menuitem", { name: "Edit startup command", exact: true }).click()
  await expect(command).toHaveValue(/long literal value/)
  await page.screenshot({ path: testInfo.outputPath("startup-desktop.png") })
})

test('untrusted Agent names render as text without running markup', async ({ page }) => {
  const payload = '<img src=x onerror="window.fakeXss=true">'
  await page.route('**/api/workspaces/ws-1/snapshot', (route) => ok(route, { ...snapshot, agents: [{ ...agentA, name: payload, path: '/' + payload }] }))
  await page.goto('/')
  await expect(page.getByRole('treeitem', { name: payload, exact: true })).toBeVisible()
  expect(await page.evaluate(() => (window as Window & { fakeXss?: boolean }).fakeXss)).toBeUndefined()
  expect(await page.locator('img[onerror]').count()).toBe(0)
})

test('authorization return rejects external, credential and fragment destinations', async ({ page }) => {
  await page.goto('/')
  const results = await page.evaluate(async () => {
    const { authorizationReturnUrl } = await import('/src/lib/control-plane.ts')
    const { proxyUrl } = await import('/src/lib/api.ts')
    const origin = new URL(proxyUrl('/')).origin
    return ['https://outside.example.test/api/apps/oauth/authorize', origin + '/api/other', origin.replace('://', '://user:fake@') + '/api/apps/oauth/authorize', origin + '/api/apps/oauth/authorize#fake', origin + '/api/apps/oauth/authorize?state=synthetic'].map((value) => {
      history.replaceState(null, '', '?return_to=' + encodeURIComponent(value))
      return authorizationReturnUrl()
    })
  })
  expect(results.slice(0, 4)).toEqual([null, null, null, null])
  expect(results[4]).toContain('/api/apps/oauth/authorize?state=synthetic')
})

for (const mobile of [false, true]) {
  test.describe(`folder expansion on ${mobile ? '390px' : 'desktop'}`, () => {
    test.use({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 } })
    const key = 'treer:folder-expansion:v1:ws-1'
    const folder = (page: Page) => page.getByRole('treeitem', { name: 'Cached folder', exact: true })
    const toggle = (page: Page) => folder(page).getByRole('button', { name: 'Cached folder', exact: true }).click()
    test.beforeEach(async ({ page }) => {
      await page.route('**/api/workspaces/ws-1/workload-tree', (route) => ok(route, {
        folders: [{ folder_id: 'cached-id', name: 'Cached folder', parent_id: null }, { folder_id: 'other-id', name: 'Other folder', parent_id: null }],
        placements: [{ agent_id: 'ag-2', folder_id: 'cached-id', position: 0 }],
      }))
    })
    test('defaults collapsed and persists both toggles immediately across reload and workspace switch', async ({ page }, testInfo) => {
      await page.goto('/')
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
      await expect(page.getByRole('treeitem', { name: 'Other folder', exact: true })).toHaveAttribute('aria-expanded', 'false')
      await toggle(page)
      expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!)['cached-id'], key)).toBe(true)
      await page.reload()
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'true')
      await page.getByRole('combobox', { name: 'Organization' }).click()
      await page.getByRole('option', { name: 'Research', exact: true }).click()
      await expect(folder(page)).toHaveCount(0)
      await page.getByRole('combobox', { name: 'Organization' }).click()
      await page.getByRole('option', { name: 'Acme', exact: true }).click()
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'true')
      await page.screenshot({ path: testInfo.outputPath('expanded.png') })
      await toggle(page)
      expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!)['cached-id'], key)).toBe(false)
      await page.reload()
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
      await page.screenshot({ path: testInfo.outputPath('collapsed.png') })
    })
    test('deleted IDs are pruned only after a successful tree response', async ({ page }) => {
      await page.addInitScript(({ key }) => localStorage.setItem(key, JSON.stringify({ 'deleted-id': true, 'cached-id': true, 'other-id': false })), { key })
      await page.goto('/')
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'true')
      await expect.poll(() => page.evaluate((key) => JSON.parse(localStorage.getItem(key)!), key)).toEqual({ 'cached-id': true, 'other-id': false })
    })
    test('rename and move retain ID preferences; deletion clears them; new folders collapse', async ({ page }) => {
      let folders = [{ folder_id: 'cached-id', name: 'Cached folder', parent_id: null as string | null }]
      await page.route('**/api/workspaces/ws-1/workload-tree', (route) => ok(route, { folders, placements: [] }))
      await page.goto('/')
      await toggle(page)
      folders = [{ folder_id: 'parent', name: 'Parent', parent_id: null }, { folder_id: 'cached-id', name: 'Renamed', parent_id: 'parent' }]
      await page.evaluate(() => window.dispatchEvent(new Event('focus')))
      await expect(page.getByRole('treeitem', { name: 'Parent', exact: true })).toHaveAttribute('aria-expanded', 'false')
      await expandFolder(page, 'Parent')
      await expect(page.getByRole('treeitem', { name: 'Renamed', exact: true })).toHaveAttribute('aria-expanded', 'true')
      folders = [{ folder_id: 'parent', name: 'Parent', parent_id: null }]
      await page.evaluate(() => window.dispatchEvent(new Event('focus')))
      await expect(page.getByRole('treeitem', { name: 'Renamed', exact: true })).toHaveCount(0)
      await expect.poll(() => page.evaluate((key) => JSON.parse(localStorage.getItem(key)!), key)).toEqual({ parent: true })
    })
    test('failed tree reads leave cached preferences intact', async ({ page }) => {
      await page.addInitScript(({ key }) => localStorage.setItem(key, JSON.stringify({ 'cached-id': true })), { key })
      let fail = true
      await page.route('**/api/workspaces/ws-1/workload-tree', (route) => fail
        ? route.fulfill({ status: 503, contentType: 'application/json', body: '{}' }) : route.fallback())
      await page.goto('/')
      await expect(page.getByRole('button', { name: 'Reload folders', exact: true })).toBeVisible()
      expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!), key)).toEqual({ 'cached-id': true })
      fail = false
      await page.getByRole('button', { name: 'Reload folders', exact: true }).click()
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'true')
    })
    test('selecting another Agent from machine overview reveals only its ancestor chain', async ({ page }) => {
      await page.route('**/api/workspaces/ws-1/workload-tree', (route) => ok(route, {
        folders: [{ folder_id: 'parent', name: 'Parent', parent_id: null }, { folder_id: 'cached-id', name: 'Cached folder', parent_id: 'parent' }, { folder_id: 'other-id', name: 'Other folder', parent_id: null }],
        placements: [{ agent_id: 'ag-2', folder_id: 'cached-id', position: 0 }],
      }))
      await page.goto('/')
      await expect(page.getByRole('treeitem', { name: 'Parent', exact: true })).toHaveAttribute('aria-expanded', 'false')
      await openWorkspaceSettings(page)
      await cloudboxRow(page).click()
      await page.locator('main section', { has: page.getByRole('heading', { name: 'Agents on this machine' }) }).getByRole('button', { name: 'Terminal', exact: true }).click()
      if (mobile) await page.getByRole('button', { name: 'Close full-screen terminal', exact: true }).click()
      await expect(page.getByRole('treeitem', { name: 'Parent', exact: true })).toHaveAttribute('aria-expanded', 'true')
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'true')
      await expect(page.getByRole('treeitem', { name: 'Other folder', exact: true })).toHaveAttribute('aria-expanded', 'false')
      await toggle(page)
      await page.reload()
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
    })
    for (const failure of ['getter', 'read', 'write']) {
      test(`storage ${failure} throws without breaking in-memory toggles`, async ({ page }) => {
        await page.addInitScript((failure) => {
          if (failure === 'getter') Object.defineProperty(window, 'localStorage', { get() { throw new Error('disabled') } })
          else Object.defineProperty(Storage.prototype, failure === 'read' ? 'getItem' : 'setItem', { value() { throw new Error('disabled') } })
        }, failure)
        await page.goto('/')
        await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
        await toggle(page)
        await expect(folder(page)).toHaveAttribute('aria-expanded', 'true')
        await toggle(page)
        await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
        await page.reload()
        await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
      })
    }
  })
}

for (const mobile of [false, true]) {
  test.describe(`trusted publication authority on ${mobile ? '390px' : 'desktop'}`, () => {
    test.use({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 } })
    const capability = 'policy.publish-authority.v1'
    const landing = '/orgs/org-1/workspaces/ws-1#policy-provider'
    const block = (page: Page) => page.getByLabel('Policy publication authority', { exact: true })
    const provider = (authority = 'owner_session') => ({ workspace_id: 'ws-1', app_id: 'policy-app', service_id: 'policy-service', failure_mode: 'fail_closed', max_stale_seconds: 300, revision_hint: 1, updated_at: NOW, updated_by: 'u1', publish_authority: authority })
    async function installFixture(page: Page, options: { owner?: boolean; capability?: boolean; authority?: string; failure?: number; malformed?: boolean } = {}) {
      let current = provider(options.authority)
      const requests: unknown[] = []
      await page.route('**/api/workspaces/ws-1/access', (route) => ok(route, { access: { ...workspaceAccess, current_role: options.owner === false ? 'member' : 'owner' } }))
      await page.route('**/api/workspaces/ws-1/policy-provider', (route) => {
        expect(route.request().method()).toBe('GET')
        return ok(route, { provider: current, capabilities: options.capability === false ? [] : [capability], cache: {} })
      })
      await page.route('**/api/workspaces/ws-1/policy-provider/publish-authority', (route) => {
        expect(route.request().method()).toBe('PUT')
        expect(route.request().headers().authorization).toBeUndefined()
        expect(route.request().headers()['x-treer-identity-token']).toBeUndefined()
        const body = route.request().postDataJSON()
        expect(body.expected_app_id).toBe('policy-app')
        expect(Object.keys(body).sort()).toEqual(['expected_app_id', 'publish_authority'])
        requests.push(body)
        if (options.failure) return route.fulfill({ status: options.failure, contentType: 'application/json', body: JSON.stringify({ error: { message: 'Core denied this owner session' } }) })
        current = provider(body.publish_authority)
        return ok(route, { provider: current, publish_authority: current.publish_authority, capabilities: options.malformed ? [] : [capability] })
      })
      return requests
    }
    test('owner confirms operator delegation then restores owner session; displays audit receipt', async ({ page, context, baseURL }, testInfo) => {
      const requests = await installFixture(page)
      await context.addCookies([{ name: 'treer_session', value: 'synthetic-owner-session', url: baseURL!, httpOnly: true, sameSite: 'Strict' }])
      await page.route('**/api/workspaces/ws-1/policy-provider/publish-authority', async (route) => {
        expect((await route.request().allHeaders()).cookie).toContain('treer_session=synthetic-owner-session')
        return route.fallback()
      })
      await page.goto(landing)
      await expect(block(page)).toContainText('owner_session')
      await block(page).getByRole('button', { name: 'Enable provider operator…', exact: true }).click()
      const dialog = page.getByRole('dialog')
      await expect(dialog).toContainText('publish v2 rules and switch to enforce without an owner approving each publication')
      await expect(dialog).toContainText('isolate the Agent that administers Policy')
      await expect(dialog.getByRole('link', { name: /Operator guide/ })).toHaveAttribute('href', /docs\/operator-guide.md#workspace-network$/)
      await page.screenshot({ path: testInfo.outputPath('operator-warning.png') })
      expect(requests).toHaveLength(0)
      await dialog.getByRole('button', { name: 'Enable provider operator', exact: true }).click()
      await expect(dialog).not.toBeVisible()
      await expect(block(page)).toContainText('Current: Provider operator')
      await expect(block(page).getByRole('status')).toContainText('policy_provider.publish_authority.set')
      await block(page).getByRole('button', { name: 'Require owner session', exact: true }).click()
      await expect(block(page)).toContainText('Current: Owner session')
      expect(requests).toEqual([{ expected_app_id: 'policy-app', publish_authority: 'provider_operator' }, { expected_app_id: 'policy-app', publish_authority: 'owner_session' }])
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
      await page.screenshot({ path: testInfo.outputPath('trusted-settings.png') })
    })
    test('trusted deep link selects the named workspace in another organization', async ({ page }) => {
      await page.route('**/api/workspaces/ws-2/policy-provider', (route) => ok(route, {
        provider: { ...provider('provider_operator'), workspace_id: 'ws-2' }, capabilities: [capability], cache: {},
      }))
      await page.goto('/orgs/org-2/workspaces/ws-2#policy-provider')
      await expect(page.getByRole('heading', { name: 'Experiments', exact: true })).toBeVisible()
      await expect(block(page)).toContainText('Current: Provider operator')
      await expect(block(page)).toContainText('Read only')
      await expect(page.locator('#policy-provider')).toBeInViewport()
      await expect(block(page).getByRole('button')).toHaveCount(0)
    })
    for (const target of ['/orgs/org-1/workspaces/missing#policy-provider', '/orgs/missing/workspaces/ws-1#policy-provider']) {
      test(`unavailable deep link never opens a different workspace: ${target}`, async ({ page }) => {
        await page.goto(target)
        await expect(page.getByText('Policy settings target is unavailable. Open a workspace you can access; no other workspace was selected.')).toBeVisible()
        await expect(block(page)).toHaveCount(0)
      })
    }
    test('canceling confirmation never writes', async ({ page }) => {
      const requests = await installFixture(page)
      await page.goto(landing)
      await block(page).getByRole('button', { name: 'Enable provider operator…', exact: true }).click()
      await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click()
      await expect(page.getByRole('dialog')).not.toBeVisible()
      await expect(block(page)).toContainText('Current: Owner session')
      expect(requests).toHaveLength(0)
    })
    for (const target of ['/orgs/org-1/workspaces/missing#policy-provider', '/orgs/missing/workspaces/ws-1#policy-provider', '/orgs/org-2/workspaces/ws-2#policy-provider']) {
      test(`live history navigation removes the previous writable settings: ${target}`, async ({ page }) => {
        const requests = await installFixture(page)
        await page.route('**/api/workspaces/ws-2/policy-provider', (route) => ok(route, {
          provider: { ...provider('provider_operator'), workspace_id: 'ws-2' }, capabilities: [capability], cache: {},
        }))
        await page.goto(landing)
        await block(page).getByRole('button', { name: 'Enable provider operator…', exact: true }).click()
        await expect(page.getByRole('dialog')).toBeVisible()
        await page.evaluate((target) => { history.pushState(null, '', target); dispatchEvent(new PopStateEvent('popstate')) }, target)
        await expect(page.getByRole('dialog')).not.toBeVisible()
        if (target.includes('missing')) {
          await expect(page.getByText('Policy settings target is unavailable. Open a workspace you can access; no other workspace was selected.')).toBeVisible()
          await expect(block(page)).toHaveCount(0)
        } else {
          await expect(block(page)).toContainText('Read only')
          await expect(block(page)).toContainText('Current: Provider operator')
          await expect(block(page).getByRole('button')).toHaveCount(0)
        }
        await page.goBack()
        await expect(block(page)).toContainText('Current: Owner session')
        await expect(block(page).getByRole('button', { name: 'Enable provider operator…', exact: true })).toBeVisible()
        expect(requests).toHaveLength(0)
      })
    }
    test('non-owner sees the current operator authority read only', async ({ page }) => {
      const requests = await installFixture(page, { owner: false, authority: 'provider_operator' })
      await page.goto(landing)
      await expect(block(page)).toContainText('Read only')
      await expect(block(page)).toContainText('Current: Provider operator')
      await expect(block(page).getByRole('button')).toHaveCount(0)
      expect(requests).toHaveLength(0)
    })
    for (const unsupported of ['missing capability', 'unknown authority']) {
      test(`old Proxy ${unsupported} is unavailable without a fallback`, async ({ page }) => {
        const requests = await installFixture(page, unsupported === 'missing capability' ? { capability: false } : { authority: 'unknown_future_value' })
        await page.goto(landing)
        await expect(block(page)).toContainText('Publication authority unavailable')
        await expect(block(page).getByRole('button')).toHaveCount(0)
        expect(requests).toHaveLength(0)
      })
    }
    test('Core rejection remains visible without changing the confirmed authority', async ({ page }) => {
      const requests = await installFixture(page, { failure: 403 })
      await page.goto(landing)
      await block(page).getByRole('button', { name: 'Enable provider operator…', exact: true }).click()
      await page.getByRole('dialog').getByRole('button', { name: 'Enable provider operator', exact: true }).click()
      await expect(page.getByRole('dialog').getByRole('alert')).toHaveText('Core denied this owner session')
      await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click()
      await expect(block(page)).toContainText('Current: Owner session')
      await expect(block(page).getByRole('alert')).toHaveText('Core denied this owner session')
      expect(requests).toHaveLength(1)
    })
    for (const response of ['missing PUT route', 'missing response capability']) {
      test(`${response} disables writes and never falls back`, async ({ page }) => {
        const requests = await installFixture(page, response === 'missing PUT route' ? { failure: 404 } : { malformed: true })
        await page.goto(landing)
        await block(page).getByRole('button', { name: 'Enable provider operator…', exact: true }).click()
        await page.getByRole('dialog').getByRole('button', { name: 'Enable provider operator', exact: true }).click()
        await expect(page.getByRole('dialog').getByRole('alert')).toContainText('no fallback was attempted')
        await expect(page.getByRole('dialog').getByRole('button', { name: 'Enable provider operator', exact: true })).toBeDisabled()
        await page.getByRole('dialog').getByRole('button', { name: 'Cancel', exact: true }).click()
        await expect(block(page)).toContainText('Publication authority unavailable')
        await expect(block(page)).not.toContainText('Publication authority changed')
        expect(requests).toHaveLength(1)
      })
    }
  })
}
