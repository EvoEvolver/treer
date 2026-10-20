import { openTabMenu, tabAction, backToWorkloads } from "./fixtures/tab-actions"
import { test, expect, type Page, type Route } from "@playwright/test"
import { browseTreeOnMobile, openOverviewMap } from './fixtures/navigation'

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

    if (path === "/invitations/pending") return route.fulfill({ status: 204 })
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
  for (const theme of ['light', 'dark']) test(`sidebar chrome: no avatar, collapse in the user row, one re-open button in the tab bar, 1px divider ${theme}`, async ({ page }, info) => {
    await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
    await page.goto('/')
    await browseTreeOnMobile(page)
    const sidebar = page.locator('#tree-sidebar')
    const collapse = sidebar.getByRole('button', { name: 'Collapse tree sidebar', exact: true })
    const expand = page.getByRole('button', { name: 'Expand tree sidebar', exact: true })
    const userMenu = sidebar.getByRole('button', { name: 'User menu' })
    // No user avatar: the user row is the collapse button, the name and the menu trigger; the email lives in the menu.
    await expect(userMenu).toContainText('Test User')
    await expect(userMenu).not.toContainText('@')
    await expect(sidebar.locator('button[aria-label="User menu"] > span').first()).not.toHaveClass(/size-8|size-7/)
    await expect(collapse).toHaveAttribute('aria-controls', 'tree-sidebar')
    await expect(expand).toHaveCount(0)
    // The divider is a single 1px line; its invisible drag zone is wider and the old arrow is gone.
    const column = await page.evaluate(() => getComputedStyle(document.querySelector('main')!).gridTemplateColumns.split(' '))
    expect(column[1]).toBe('1px')
    expect((await separator(page).boundingBox())!.width).toBeGreaterThanOrEqual(20)
    await expect(page.locator('main > div button')).toHaveCount(0)
    await page.screenshot({ path: info.outputPath(`sidebar-chrome-${theme}-expanded.png`) })
    // The user menu stays reachable from the user row.
    await userMenu.click()
    await expect(page.getByRole('menuitem', { name: 'Settings' })).toBeVisible()
    await expect(page.getByRole('menuitem', { name: 'Log out' })).toBeVisible()
    await expect(page.getByRole('menu')).toContainText('@')
    await page.keyboard.press('Escape')
    // Collapse from the user row; focus lands on the single re-open button in the tab bar.
    await collapse.click()
    await expect(page.locator('#tree-sidebar')).toBeHidden()
    await expect(expand).toBeFocused()
    await expect(expand).toHaveAttribute('aria-expanded', 'false')
    await expect(page.getByRole('button', { name: 'Collapse tree sidebar', exact: true })).toHaveCount(0)
    await expect(page.locator('main > div')).toHaveCSS('width', '0px')
    await page.screenshot({ path: info.outputPath(`sidebar-chrome-${theme}-collapsed.png`) })
    await page.reload()
    await browseTreeOnMobile(page)
    // The tab bar remounts once the workspace has loaded; focus the re-open button only after that.
    await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
    await expect(page.locator('[role=treeitem]').first()).toBeAttached()
    await expect(expand).toBeVisible()
    await expect(async () => { await expand.focus(); await expect(expand).toBeFocused({ timeout: 500 }) }).toPass()
    await page.keyboard.press('Space')
    await expect(sidebar).toBeVisible()
    await expect(collapse).toBeFocused()
    await expect(expand).toHaveCount(0)
    // Phones keep the page-style sidebar: no collapse button, the user menu is still one tap away at 44px.
    await page.setViewportSize({ width: 390, height: 844 })
    await browseTreeOnMobile(page)
    await expect(collapse).toBeHidden()
    const phoneMenu = sidebar.getByRole('button', { name: 'User menu' })
    expect((await phoneMenu.boundingBox())!.height).toBeGreaterThanOrEqual(44)
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: info.outputPath(`sidebar-chrome-${theme}-390.png`) })
  })
  // Guard, not a fix: Chromium already maps the transparent thumb to a system colour in forced-colors mode.
  // This fails if someone later opts the tree out with forced-color-adjust: none.
  test('forced colors keep a visible tree scrollbar thumb', async ({ page }) => {
    await page.emulateMedia({ forcedColors: 'active' })
    await page.goto('/')
    await browseTreeOnMobile(page)
    const tree = page.getByRole('tree', { name: 'Workspace workloads' })
    await expect(tree.getByRole('treeitem').first()).toBeVisible()
    await expect(tree).not.toHaveAttribute('data-scrolling', '', { timeout: 3000 })
    const thumb = await tree.evaluate((element) => getComputedStyle(element, '::-webkit-scrollbar-thumb').backgroundColor)
    expect(thumb).not.toBe('rgba(0, 0, 0, 0)')
    expect(thumb).not.toBe('transparent')
  })
  async function platform(page: Page, name: 'MacIntel' | 'Win32') {
    // userAgentData.platform wins over navigator.platform in the app, so hide it to make the override effective.
    await page.addInitScript((value) => { Object.defineProperty(navigator, 'platform', { get: () => value }); Object.defineProperty(navigator, 'userAgentData', { get: () => undefined }) }, name)
  }
  // Records whether the page's own keydown handling called preventDefault, i.e. whether the Console claimed the key.
  async function claimed(page: Page, press: string) {
    await page.evaluate(() => { (window as unknown as { __claimed: boolean[] }).__claimed = []; window.addEventListener('keydown', (event) => queueMicrotask(() => (window as unknown as { __claimed: boolean[] }).__claimed.push(event.defaultPrevented))) })
    await page.keyboard.press(press)
    return page.evaluate(() => (window as unknown as { __claimed: boolean[] }).__claimed.at(-1) ?? false)
  }
  const sidebarHidden = (page: Page) => expect(page.locator('#tree-sidebar')).toBeHidden()
  const sidebarShown = (page: Page) => expect(page.locator('#tree-sidebar')).toBeVisible()
  for (const [name, key, other] of [['MacIntel', 'Meta+b', 'Control+b'], ['Win32', 'Control+b', 'Meta+b']] as const) test(`${key} toggles the sidebar like the collapse button on ${name}; the other modifier does nothing`, async ({ page }) => {
    await platform(page, name)
    await page.goto('/')
    await browseTreeOnMobile(page)
    // The tab bar remounts after the workspace loads; wait for the loaded layout instead of sleeping.
    await expect(page.getByRole('heading', { name: 'Workspace overview' })).toBeVisible()
    await expect(page.locator('[role=treeitem]').first()).toBeAttached()
    await expect(page.locator('#tree-sidebar').getByRole('button', { name: 'Collapse tree sidebar' })).toBeVisible()
    expect(await claimed(page, other)).toBe(false)
    await sidebarShown(page)
    expect(await claimed(page, key)).toBe(true)
    await sidebarHidden(page)
    await expect(page.getByRole('button', { name: 'Expand tree sidebar', exact: true })).toBeVisible()
    expect(await page.evaluate(() => localStorage.getItem('treer:tree-collapsed:v1:u1'))).toBe('true')
    // Persists like the button: a reload stays collapsed; the shortcut brings it back.
    await page.reload()
    await expect(page.getByRole('button', { name: 'Expand tree sidebar', exact: true })).toBeVisible()
    await page.keyboard.press(key)
    await sidebarShown(page)
    expect(await page.evaluate(() => localStorage.getItem('treer:tree-collapsed:v1:u1'))).toBe('false')
    // Extra modifiers and other keys are not ours.
    expect(await claimed(page, key.replace('+b', '+Shift+b'))).toBe(false)
    await sidebarShown(page)
  })
  test('the shortcut is not captured inside a terminal or a rich-text editable, and does not steal focus from the page', async ({ page }) => {
    await platform(page, 'Win32')
    await page.goto('/')
    await browseTreeOnMobile(page)
    await page.getByRole('link', { name: /^api-server(\s|$)/ }).click()
    await expect(page.locator('.xterm')).toBeVisible()
    await page.locator('.xterm-helper-textarea').focus()
    expect(await claimed(page, 'Control+b')).toBe(false)
    await sidebarShown(page)
    // A contenteditable (bold shortcut) keeps the key too.
    await page.evaluate(() => { const editor = document.createElement('div'); editor.id = 'rich'; editor.contentEditable = 'true'; editor.tabIndex = 0; editor.textContent = 'x'; document.body.appendChild(editor); editor.focus() })
    expect(await claimed(page, 'Control+b')).toBe(false)
    await sidebarShown(page)
    // Focus outside the sidebar stays where it is when the shortcut collapses it.
    await page.evaluate(() => { document.getElementById('rich')!.remove(); (document.querySelector('[role=tablist] [role=tab]') as HTMLElement).focus() })
    const tab = await page.evaluate(() => document.activeElement?.getAttribute('role'))
    expect(tab).toBe('tab')
    expect(await claimed(page, 'Control+b')).toBe(true)
    await sidebarHidden(page)
    expect(await page.evaluate(() => document.activeElement?.getAttribute('role'))).toBe('tab')
  })
  for (const [lang, name, text] of [['en', 'MacIntel', 'Drag to resize · ⌘B to hide sidebar'], ['en', 'Win32', 'Drag to resize · Ctrl+B to hide sidebar'], ['zh', 'MacIntel', '拖动调整宽度 · ⌘B 隐藏侧边栏']] as const) test(`the resize handle teaches the shortcut (${lang}, ${name})`, async ({ page }, info) => {
    await platform(page, name)
    await page.addInitScript((value) => localStorage.setItem('treer-lang', value), lang)
    await page.goto('/')
    await browseTreeOnMobile(page)
    const handle = page.getByRole('separator')
    await handle.hover()
    await expect(page.getByRole('tooltip')).toHaveText(text)
    await page.screenshot({ path: info.outputPath(`shortcut-hint-${lang}-${name}.png`) })
    // Dragging closes the hint; the collapse button names the shortcut too.
    const box = (await handle.boundingBox())!
    await page.mouse.down(); await page.mouse.move(box.x + box.width / 2 + 20, box.y + 200, { steps: 4 })
    await expect(page.getByRole('tooltip')).toHaveCount(0)
    await page.mouse.up()
    const collapse = page.locator('#tree-sidebar button[aria-keyshortcuts]')
    await expect(collapse).toHaveAttribute('aria-keyshortcuts', name === 'MacIntel' ? 'Meta+B' : 'Control+B')
    await expect(collapse).toHaveAttribute('title', new RegExp(name === 'MacIntel' ? '⌘B\\)$' : 'Ctrl\\+B\\)$'))
  })
  test('on a phone the shortcut is ignored (no sidebar to toggle, key not claimed)', async ({ page }) => {
    await platform(page, 'Win32')
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto('/')
    await browseTreeOnMobile(page)
    expect(await claimed(page, 'Control+b')).toBe(false)
    await expect(page.locator('#tree-sidebar')).toBeVisible()
  })
  test('the tree scrollbar is hidden until it scrolls, then fades', async ({ page }) => {
    await page.goto('/')
    await browseTreeOnMobile(page)
    const tree = page.getByRole('tree', { name: 'Workspace workloads' })
    await expect(tree).toHaveClass(/thin-scroll/)
    await expect(tree.getByRole('treeitem').first()).toBeVisible()
    await expect(tree).not.toHaveAttribute('data-scrolling', '', { timeout: 3000 })
    // Make the tree overflow without touching React's children: shrink the element itself.
    await tree.evaluate((element) => { element.style.maxHeight = '90px'; element.scrollTop = 40 })
    await expect(tree).toHaveAttribute('data-scrolling', '')
    await expect(tree).not.toHaveAttribute('data-scrolling', '', { timeout: 3000 })
    // Keyboard scrolling is untouched: a focused row plus PageDown really scrolls the (still 90px high) tree.
    await tree.evaluate((element) => { element.scrollTop = 0 })
    await expect.poll(() => tree.evaluate((element) => element.scrollTop)).toBe(0)
    await tree.getByRole('link').first().focus()
    await expect(tree.getByRole('link').first()).toBeFocused()
    await page.keyboard.press('PageDown')
    await expect.poll(() => tree.evaluate((element) => element.scrollTop)).toBeGreaterThan(0)
  })
  async function drag(page: Page, delta: number) {
    const box = (await separator(page).boundingBox())!
    await page.mouse.move(box.x + box.width / 2, box.y + 200)
    await page.mouse.down()
    await page.mouse.move(box.x + box.width / 2 + delta, box.y + 200, { steps: 8 })
  }
  test('drag persists across refresh/workspaces, protects terminal, and double click resets', async ({ page }, testInfo) => {
    const resizeColumns: number[] = []
    let lastResizeAt = 0
    let initialColumns = 0
    // Dragging emits intermediate sizes. Capture the baseline only after the
    // final layout/fit has settled, rather than the first smaller resize.
    async function settledColumns() {
      let columns = 0
      await expect.poll(() => {
        columns = resizeColumns.at(-1) ?? 0
        return columns > 0 && Date.now() - lastResizeAt >= 300
      }).toBe(true)
      return columns
    }
    await page.routeWebSocket(/\/agents\/[^/]+\/terminal(?:\?.*)?$/, (ws) => {
      initialColumns = Number(new URL(ws.url()).searchParams.get('cols'))
      ws.onMessage((message) => { if (typeof message === 'string') { const control = JSON.parse(message); if (control.type === 'resize') { resizeColumns.push(control.cols); lastResizeAt = Date.now() } } })
      ws.send(JSON.stringify({ type: 'ready', stream_epoch: 'resize', revision: 0, replay_chunks: 0 }))
    })
    await page.goto('/')
    await browseTreeOnMobile(page)
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '272')
    await page.getByRole('link', { name: /^api-server(\s|$)/ }).click()
    await expect.poll(() => initialColumns).toBeGreaterThan(0)
    await drag(page, 96)
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '368')
    await expect(page.locator('main > section')).toHaveCSS('pointer-events', 'none')
    await page.mouse.up()
    await expect(page.locator('main > section')).toHaveCSS('pointer-events', 'auto')
    const expandedColumns = await settledColumns()
    expect(expandedColumns).toBeLessThan(initialColumns)
    await page.getByRole('button', { name: 'Collapse tree sidebar', exact: true }).click()
    await expect.poll(() => resizeColumns.at(-1)!).toBeGreaterThan(expandedColumns)
    expect(await settledColumns()).toBeGreaterThan(expandedColumns)
    await page.getByRole('button', { name: 'Expand tree sidebar', exact: true }).click()
    await expect.poll(() => resizeColumns.at(-1)!).toBe(expandedColumns)
    expect(await settledColumns()).toBe(expandedColumns)
    expect(await page.evaluate((key) => localStorage.getItem(key), key)).toBe('368')
    await page.screenshot({ path: testInfo.outputPath('tree-resizer-desktop.png') })
    await page.reload()
    await browseTreeOnMobile(page)
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '368')
    await page.goto('/orgs/org-2/workspaces/ws-2')
    await browseTreeOnMobile(page)
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '368')
    await separator(page).dblclick({ position: { x: 12, y: 120 } })
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '272')
    await page.reload()
    await browseTreeOnMobile(page)
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '272')
  })
  test('collapse is keyboard accessible and persists without losing width or mobile navigation', async ({ page }, testInfo) => {
    await page.goto('/')
    await browseTreeOnMobile(page)
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
    await browseTreeOnMobile(page)
    await expect(expand).toBeVisible()
    await expect(page.locator('#tree-sidebar')).toBeHidden()
    await page.goto('/orgs/org-2/workspaces/ws-2')
    await browseTreeOnMobile(page)
    await expect(expand).toBeVisible()
    await page.setViewportSize({ width: 390, height: 844 })
    await browseTreeOnMobile(page)
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
    await browseTreeOnMobile(page)
    await expect(collapse).toBeVisible()
    await expect(page.locator('#tree-sidebar')).toBeVisible()
  })
  test('keyboard exposes separator values, clamps on resize and stays hidden at 390px', async ({ page }, testInfo) => {
    await page.goto('/')
    await browseTreeOnMobile(page)
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
    await browseTreeOnMobile(page)
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
      await browseTreeOnMobile(page)
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
    await browseTreeOnMobile(page)
    await expect(separator(page)).toHaveAttribute('aria-valuenow', '272')
    await separator(page).press('ArrowRight')
    expect(await page.evaluate(() => localStorage.getItem('treer:tree-width:v1:other-user'))).toBe('480')
    await page.getByRole('button', { name: 'Collapse tree sidebar', exact: true }).click()
    await page.getByRole('button', { name: 'Expand tree sidebar', exact: true }).click()
    expect(await page.evaluate(() => localStorage.getItem('treer:tree-collapsed:v1:other-user'))).toBe('true')
  })
  test('touch pointer drag resizes on a large touch screen', async ({ page, context }) => {
    await page.goto('/')
    await browseTreeOnMobile(page)
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
    await browseTreeOnMobile(page)
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
      await browseTreeOnMobile(page)
      await page.getByRole("button", { name: "Actions for api-server", exact: true }).click()
      await page.getByRole("menuitem", { name: "Edit startup command", exact: true }).click()
      const dialog = page.getByRole("dialog")
      await expect(dialog.getByRole("textbox", { name: "Startup command", exact: true })).toHaveValue("codex resume original-thread 'literal value'")
      await expect(dialog.getByRole("textbox", { name: "Startup working directory" })).toHaveValue("repo")
      await dialog.getByRole("textbox", { name: "Startup command", exact: true }).fill("codex --no-daemon resume original-thread 'two words'")
      await dialog.getByRole("textbox", { name: "Startup working directory" }).fill("other repo")
      await dialog.getByRole("button", { name: "Save and restart" }).click()
      await expect(dialog).not.toBeVisible()
      await expect(page.locator("[data-tour=agents-tab]").getByRole("status")).toContainText("Saved startup command and restarted /api-server")
      expect(replacements).toBe(1)
      await page.getByRole("button", { name: "Actions for Soul Archive", exact: true }).click()
      await expect(page.getByRole("menuitem", { name: "Edit startup command", exact: true })).toHaveCount(0)
    })
    test("keeps errors visible and never restarts after an unsupported read", async ({ page }) => {
      let replacements = 0
      await page.route("**/agents/ag-1/admin/startup", (route) => route.fulfill({ status: 400, contentType: "application/json", body: JSON.stringify({ error: { code: "unsupported_command", message: "Controller requires agent.startup-admin.v1" } }) }))
      await page.route("**/agents/ag-1/restart-with-startup", (route) => { replacements++; return ok(route, {}) })
      await page.goto("/")
      await browseTreeOnMobile(page)
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
      await browseTreeOnMobile(page)
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
      await browseTreeOnMobile(page)
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
const workstationRow = (page: Page) => page.getByRole("button", { name: /^workstation(\s|$)/ })
const cloudboxRow = (page: Page) => page.getByRole("button", { name: /^cloudbox(\s|$)/ })
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
  await browseTreeOnMobile(page)
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

    for (const theme of ["light", "dark"] as const) {
      test(`Overview Add agent creates at root without a Tree folder label (${theme})`, async ({ page }, testInfo) => {
        await page.addInitScript((theme) => localStorage.setItem("treer-theme", theme), theme)
        await page.goto("/")
        const overview = page.locator("#workspace-panel-overview")
        await expect(overview.getByRole("heading", { name: "Workspace overview", exact: true })).toBeVisible()
        await overview.getByRole("button", { name: "Add agent", exact: true }).click()
        const dialog = page.getByRole("dialog")
        await expect(dialog.getByRole("heading", { name: "Create agent", exact: true })).toBeVisible()
        await expect(dialog).not.toContainText("Tree folder:")
        await page.screenshot({ path: testInfo.outputPath("overview-create-root.png") })
      })
    }

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
        if (mobile) await backToWorkloads(page)
        await expect(page.getByRole("treeitem", { name: "experiments", exact: true }).getByRole("treeitem", { name: "reviewer", exact: true })).toBeVisible()
        await page.reload()
        await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await expect(page.locator("aside").getByText("Acme")).toBeVisible()
  await expect(page.getByRole("combobox", { name: "Workspace" })).toHaveText("Demo")
  await expect.poll(() => new URL(page.url()).pathname).toBe("/orgs/org-1/workspaces/ws-1")
  await expect(page).toHaveTitle("Acme / Demo")
  await expect(page.getByRole("tree", { name: "Workspace workloads" })).toBeVisible()
  await expect(page.locator("aside [role=tab]")).toHaveCount(0)
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toHaveCount(1)
  await expect(page.getByRole("link", { name: /^api-server(\s|$)/ })).toBeVisible()
  await expect(page.getByRole("link", { name: /^worker(\s|$)/ })).toBeVisible()
  await expect(page.getByRole("link", { name: /^app:Soul Archive(\s|$)/ })).toHaveCount(0)
})

test("mixed workloads use common lifecycle endpoints and logs", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
  for (const [name, id] of [["api-server", "ag-1"], ["Soul Archive", "appw-1"]]) {
    for (const action of ["Start", "Stop", "Restart"]) {
      await page.getByRole("button", { name: `Actions for ${name}`, exact: true }).click()
      const request = page.waitForRequest((request) => request.url().endsWith(`/agents/${id}/${action.toLowerCase()}`) && request.method() === "POST")
      await page.getByRole("menuitem", { name: action, exact: true }).click()
      // Stop is destructive and asks first; Start and Restart act immediately.
      if (action === "Stop") await page.getByRole("dialog").getByRole("button", { name: "Stop", exact: true }).click()
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
  await browseTreeOnMobile(page)
  // The Tree row is one line; the icon carries the kind (agent vs App).
  const treeRow = (name: RegExp) => page.locator('#tree-sidebar').getByRole("link", { name })
  await expect(treeRow(/^api-server(\s|$)/).locator('[data-workload-icon=agent]')).toBeVisible()
  await expect(treeRow(/^worker(\s|$)/).locator('[data-workload-icon=agent]')).toBeVisible()
  await expect(treeRow(/^Soul Archive(\s|$)/).locator('[data-workload-icon=app]')).toBeVisible()
  await openOverviewMap(page)
  const app = page.getByRole("dialog").getByRole("link", { name: /^Open \/Soul Archive · app ·/ })
  await expect(app).toBeVisible()
  await expect(app).toHaveAttribute("title", / · app ·/)
  await expect(page.getByRole("dialog").getByRole("link", { name: /^Open \/api-server · agent ·/ })).toBeVisible()
})

test("folder loading failure keeps workloads usable and supports retry", async ({ page }) => {
  let fail = true
  await page.route("**/api/workspaces/ws-1/workload-tree", async (route) => {
    if (fail) return route.fulfill({ status: 503, contentType: "application/json", body: JSON.stringify({ error: "Temporarily unavailable" }) })
    return ok(route, { folders: [{ folder_id: "restored", name: "Recovered folder", parent_id: null }], placements: [] })
  })
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toHaveCount(1)
  await page.getByRole("button", { name: "Actions for Soul Archive", exact: true }).click()
  await page.getByRole("menuitem", { name: "Logs", exact: true }).click()
  await expect(page.getByRole("treeitem", { name: "Soul Archive", exact: true })).toHaveAttribute("aria-selected", "true")
  await expect(page.getByRole("tab", { name: "Tab /Soul Archive", exact: true })).toHaveAttribute("aria-selected", "true")
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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await expect(page.getByRole("treeitem")).toHaveCount(3)
  await expect.poll(() => page.getByRole("treeitem").evaluateAll((rows) => rows.map((row) => row.getAttribute("aria-label")))).toEqual(["api-server", "Soul Archive", "worker"])
})

test("dragging workloads before and after siblings saves order across reloads", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
  const row = (name: string) => page.getByRole("treeitem", { name, exact: true })
  const names = () => page.getByRole("treeitem").evaluateAll((rows) => rows.map((item) => item.getAttribute("aria-label")))
  await expect.poll(names).toEqual(["api-server", "Soul Archive", "worker"])
  await row("worker").dragTo(row("api-server"), { targetPosition: { x: 20, y: 4 } })
  await expect.poll(names).toEqual(["worker", "api-server", "Soul Archive"])
  await page.reload()
  await browseTreeOnMobile(page)
  await expect.poll(names).toEqual(["worker", "api-server", "Soul Archive"])
  const bottom = (await row("Soul Archive").boundingBox())!.height - 4
  await row("worker").dragTo(row("Soul Archive"), { targetPosition: { x: 20, y: bottom } })
  await expect.poll(names).toEqual(["api-server", "Soul Archive", "worker"])
  await page.reload()
  await browseTreeOnMobile(page)
  await expect.poll(names).toEqual(["api-server", "Soul Archive", "worker"])
})

test("mobile workload menus move entries up and down and disable list boundaries", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await expect.poll(names).toEqual(["Soul Archive", "worker", "api-server"])
})

test("dragging into a folder inserts at the chosen sibling and preserves that order", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await expect.poll(names).toEqual(["worker", "api-server", "Soul Archive"])
})

test("folders support persistent drag and keyboard moves, nesting, rename and deletion", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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
  await page.getByRole("treeitem", { name: "Soul Archive", exact: true }).getByRole("link", { name: /^Soul Archive(\s|$)/ }).click()
  await expect(page.getByRole('button', { name: /^Page .*; long-press for actions$/ })).toBeVisible()
  await backToWorkloads(page)
  await expect(page.getByRole("tree", { name: "Workspace workloads" })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

test("restores organization and workspace from the URL after reload", async ({ page }) => {
  await page.goto("/orgs/org-2/workspaces/ws-2?source=bookmark")
  await browseTreeOnMobile(page)
  await expect(page.getByRole("combobox", { name: "Organization" })).toHaveText("Research")
  await expect(page.getByRole("combobox", { name: "Workspace" })).toHaveText("Experiments")
  await expect.poll(() => new URL(page.url()).searchParams.get("source")).toBe("bookmark")
  await expect(page).toHaveTitle("Research / Experiments")

  await page.reload()
  await browseTreeOnMobile(page)

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
  await browseTreeOnMobile(page)
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
  await expect(page.getByRole("link", { name: /^api-server(\s|$)/ })).toHaveCount(0)
})

test("agent list row menu can rename and delete", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
  await agentsTab(page).click()
  const row = page.getByRole("link", { name: /^api-server(\s|$)/ })
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
  await browseTreeOnMobile(page)
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await expect(page.getByText("Set up a machine first", { exact: true })).toBeVisible()
  const addMachinePrompt = page.getByRole("menuitem", { name: "Set up a machine before creating an agent" })
  await expect(addMachinePrompt).toBeEnabled()
  await addMachinePrompt.click()
  await expect(page.getByText("Add a machine to create agents", { exact: true })).toBeVisible()
  await expect(page.getByText("Agents run on machines.", { exact: false })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Create agent" })).toHaveCount(0)
  await page.getByRole("tabpanel", { name: "Settings", exact: true }).getByRole("button", { name: "Set up a machine", exact: true }).click()
  await expect(page.getByRole("dialog").getByRole("heading", { name: "Add machine", exact: true })).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})

for (const mobile of [false, true]) for (const theme of ['light', 'dark']) test(`empty Tree offers machine setup without opening a menu (${theme}, ${mobile ? '390px' : 'desktop'})`, async ({ page }, info) => {
  await page.setViewportSize({ width: mobile ? 390 : 1360, height: 844 })
  await page.addInitScript((value) => localStorage.setItem('treer-theme', value), theme)
  await page.route("**/api/workspaces/ws-1/bootstrap", (route) => ok(route, { install_command: "echo synthetic install", connect_command: "echo synthetic connect" }))
  await page.route("**/api/workspaces/ws-1/snapshot", (route) => ok(route, { ...snapshot, servers: [], agents: [] }))
  await page.goto("/")
  await browseTreeOnMobile(page)
  const sidebar = page.locator('#tree-sidebar')
  await expect(sidebar.getByText('No machines in this workspace.', { exact: false })).toBeVisible()
  const setup = sidebar.getByRole('button', { name: 'Set up a machine', exact: true })
  await expect(setup).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: info.outputPath(`machine-guide-${theme}-${mobile ? '390' : '1360'}.png`) })
  await expect.poll(async () => { await setup.focus(); return setup.evaluate((element) => element === document.activeElement) }).toBe(true)
  await setup.press('Enter')
  await expect(page.getByText('Add a machine to create agents', { exact: true })).toBeVisible()
  await page.getByRole('tabpanel', { name: 'Settings', exact: true }).getByRole('button', { name: 'Set up a machine', exact: true }).click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: 'Add machine', exact: true })).toBeVisible()
})

test('Tree hides the machine guide when a machine is enrolled', async ({ page }) => {
  await page.goto('/')
  await browseTreeOnMobile(page)
  await expect(page.locator('#tree-sidebar').getByRole('button', { name: 'Set up a machine', exact: true })).toHaveCount(0)
})

test("organization actions open a settings page with inline management", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await expect(page.getByRole("button", { name: "Invite member" }).locator("svg")).toHaveClass(/lucide-plus/)

  await expect(page.getByLabel("Organization name")).toBeEnabled()
  await page.getByRole("button", { name: "Select Demo workspace" }).click()
  await expect(page.getByRole("heading", { name: "General" })).toBeHidden()
})

test("mobile organization settings hide the sidebar and fit the viewport", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto("/")
  await browseTreeOnMobile(page)
  await page.getByRole("button", { name: "Organization settings" }).click()

  await expect(page.locator("aside")).toBeHidden()
  await expect(page.getByRole("heading", { name: "Acme", exact: true })).toBeVisible()
  await expect(page.getByRole("button", { name: "Back" })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

test("audit view presents versioned traffic as billable usage", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
  await page.getByRole("button", { name: "Organization settings" }).click()
  await page.getByRole("button", { name: "Open audit" }).click()

  await expect(page.getByRole("group", { name: "Traffic totals" }).getByText("Billable usage", { exact: true })).toBeVisible()
  await expect(page.getByRole("group", { name: "Traffic totals" }).getByText("Data frames", { exact: true })).toBeVisible()
  await expect(page.getByText("Machine", { exact: true })).toHaveCount(2)
})

test("workspace settings contain Machines and launch profiles without a separate views menu", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await openWorkspaceSettings(page)
  await expect(page.getByRole("button", { name: "Install Default Policy App" })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

test("workspace controls open a settings page with inline rename", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await page.getByRole("button", { name: "Actions for Soul Archive" }).click()
  await page.getByRole("menuitem", { name: "App settings", exact: true }).click()

  await expect(page.getByRole("heading", { name: "Soul Archive" })).toBeVisible()
  await expect(page.locator("aside")).toBeHidden()
  await expect(page.getByRole("button", { name: "Back" })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

test("clicking a machine opens an overview with identity, agents, services, virtual hosts, and traffic", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await page.getByRole("link", { name: /^api-server(\s|$)/ }).click()
  await tabAction(page, 'Upload file')
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
  await browseTreeOnMobile(page)
  await page.getByRole("link", { name: /^api-server(\s|$)/ }).click()
  await tabAction(page, 'Upload file')

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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await openWorkspaceSettings(page)
  await expect(page.getByText("workstation · srv-b :8794")).toBeVisible()
  await page.getByRole("button", { name: /^workstation(\s|$)/ }).first().click()
  await expect(page.getByText("treer-agent-server service --workspace ws-1 restart-controller")).toBeVisible()
  await expect(page.getByRole("button", { name: "Copy restart-controller" })).toBeVisible()
  await expect(page.getByRole("button", { name: "Copy restore" })).toBeVisible()
})

test("machine overview only shows agents for the selected machine", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await openWorkspaceSettings(page)
  await workstationRow(page).click()
  await page.getByRole("button", { name: "Terminal", exact: true }).first().click()

  // Returns to the terminal main view; the selected tab identifies its path.
  await expect(page.getByRole("tab", { name: "Tab /api-server", exact: true })).toHaveAttribute("aria-selected", "true")
})

test("terminal drains a burst of small output frames without stalling", async ({ page }) => {
  // Setup sends 1,600 WebSocket messages over the browser driver and can exceed
  // the default test budget on the shared machine. Keep the drain deadline below
  // at two seconds so this remains a regression check for a stalled output queue.
  test.setTimeout(60_000)
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
  await browseTreeOnMobile(page)
  await page.getByRole("link", { name: /^api-server(\s|$)/ }).click()

  await expect(page.locator(".xterm-rows")).toContainText(`burst-frame-${frameCount}`, { timeout: 2_000 })
  await expect.poll(() => acknowledgedBytes).toBe(outputBytes)
})

for (const mobile of [false, true]) {
  test.describe(`terminal replay queries on ${mobile ? '390px' : 'desktop'}`, () => {
    test.use({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 } })
    const queries = '\x1b]10;?\x07\x1b]11;?\x1b\\\x1b[c\x1b[>c\x1b[6n\x1b[?6n\x1b[5n\x1b]12;?\x07\x1b]4;1;?\x07\x1b[4$p\x1b[?25$p\x1bP$qm\x1b\\\x1b[?15n\x1b[?25n\x1b[?26n\x1b[?53n'
    async function open(page: Page) {
      await page.goto('/')
      await browseTreeOnMobile(page)
      await page.getByRole('link', { name: /^api-server(\s|$)/ }).click()
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
      await browseTreeOnMobile(page)
      await page.getByRole('link', { name: /^api-server(\s|$)/ }).click()
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
  await browseTreeOnMobile(page)
  await page.getByRole("link", { name: /^api-server(\s|$)/ }).click()

  await expect.poll(() => page.evaluate(() => (window as unknown as { __terminalCloseCodes: number[] }).__terminalCloseCodes)).toContain(4001)
  expect(pageErrors).toEqual([])
})

test("create agent dialog offers profiles and explicit commands without provider installation", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
  await agentsTab(page).click()
  await page.getByRole("button", { name: "Add to tree", exact: true }).click()
  await page.getByRole("menuitem", { name: "Add agent", exact: true }).click()
  await page.getByRole("dialog").getByRole("combobox", { name: "Launch" }).click()
  await expect(page.getByRole("option", { name: "Terminal", exact: true })).toBeVisible()
  await expect(page.getByRole("option", { name: "Custom command", exact: true })).toBeVisible()
  await expect(page.getByRole("option", { name: /^Install(\s|$)/ })).toHaveCount(0)
  await page.getByRole("option", { name: "Codex", exact: true }).click()
  await expect(page.getByRole("button", { name: "Create agent" })).toBeVisible()
})

test("create agent custom command preserves argv without a provider installer", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await openWorkspaceSettings(page)
  await workstationRow(page).click()
  await expect(page.getByRole("heading", { name: "workstation" })).toBeVisible()
  await expect(page.locator("aside")).toBeHidden()
  await page.getByRole("button", { name: "Back" }).dispatchEvent("click")
  await expect(page.getByRole("heading", { name: "Launch profiles" })).toBeVisible()
  await expect(page.locator("aside")).toBeHidden()
  await page.getByRole("button", { name: "Back" }).dispatchEvent("click")
  await expect(page.getByRole("heading", { name: "Workspace overview", exact: true })).toBeVisible()
  await browseTreeOnMobile(page)
  await expect(page.locator("aside")).toBeVisible()
})

async function openUserMenu(page: Page) {
  await page.getByRole("button", { name: "User menu" }).click()
}

test("user menu contains Settings and Log out, not Edit profile", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
  await openUserMenu(page)
  await expect(page.getByRole("menuitem", { name: "Settings" })).toBeVisible()
  await expect(page.getByRole("menuitem", { name: "Log out" })).toBeVisible()
  await expect(page.getByRole("menuitem", { name: "Edit profile" })).toHaveCount(0)
})

test("opening Settings shows a floating panel with Usage & billing, Account, and General", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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

test("General settings show theme and the language choice, and toggle dark class", async ({ page }) => {
  await page.goto("/")
  await browseTreeOnMobile(page)
  await openUserMenu(page)
  await page.getByRole("menuitem", { name: "Settings" }).click()
  const settings = page.getByRole("dialog", { name: "Settings" })
  await settings.getByRole("navigation", { name: "Settings" }).getByRole("button", { name: "General" }).click()

  await expect(settings.getByRole("group", { name: "Theme" })).toBeVisible()
  await expect(settings.getByRole("button", { name: "Light" })).toBeVisible()
  await expect(settings.getByRole("button", { name: "Dark" })).toBeVisible()
  await expect(settings.getByLabel("Language")).toHaveText("English")
  await expect(settings.getByText("Applies to the console. Embedded interfaces follow it when they load.")).toBeVisible()

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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
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
      await browseTreeOnMobile(page)
      await expandFolder(page, "research")
      await expandFolder(page, "delivery")
      const first = page.locator('[data-agent-id="ag-1"]')
      const second = page.locator('[data-agent-id="ag-2"]')
      await expect(first).toHaveAttribute("title", "/research/reviewer")
      await expect(second).toHaveAttribute("title", "/delivery/reviewer")
      for (const [row, path] of [[first, "/research/reviewer"], [second, "/delivery/reviewer"]] as const) {
        await row.getByRole("link").first().click()
        await expect(row).toHaveAttribute("aria-selected", "true")
        if (mobile) {
          await expect(page.getByRole("tabpanel").locator(`span[title="${path}"]`)).toBeVisible()
          await backToWorkloads(page)
        } else await expect(page.getByRole("tab", { name: `Tab ${path}`, exact: true })).toHaveAttribute("title", path)
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
  await browseTreeOnMobile(page)
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
  await browseTreeOnMobile(page)
  await expect(page.getByRole('treeitem', { name: payload, exact: true })).toBeVisible()
  expect(await page.evaluate(() => (window as Window & { fakeXss?: boolean }).fakeXss)).toBeUndefined()
  expect(await page.locator('img[onerror]').count()).toBe(0)
})

test('authorization return rejects external, credential and fragment destinations', async ({ page }) => {
  await page.goto('/')
  await browseTreeOnMobile(page)
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
      await browseTreeOnMobile(page)
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
      await expect(page.getByRole('treeitem', { name: 'Other folder', exact: true })).toHaveAttribute('aria-expanded', 'false')
      await toggle(page)
      expect(await page.evaluate((key) => JSON.parse(localStorage.getItem(key)!)['cached-id'], key)).toBe(true)
      await page.reload()
      await browseTreeOnMobile(page)
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
      await browseTreeOnMobile(page)
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
      await page.screenshot({ path: testInfo.outputPath('collapsed.png') })
    })
    test('deleted IDs are pruned only after a successful tree response', async ({ page }) => {
      await page.addInitScript(({ key }) => localStorage.setItem(key, JSON.stringify({ 'deleted-id': true, 'cached-id': true, 'other-id': false })), { key })
      await page.goto('/')
      await browseTreeOnMobile(page)
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'true')
      await expect.poll(() => page.evaluate((key) => JSON.parse(localStorage.getItem(key)!), key)).toEqual({ 'cached-id': true, 'other-id': false })
    })
    test('rename and move retain ID preferences; deletion clears them; new folders collapse', async ({ page }) => {
      let folders = [{ folder_id: 'cached-id', name: 'Cached folder', parent_id: null as string | null }]
      await page.route('**/api/workspaces/ws-1/workload-tree', (route) => ok(route, { folders, placements: [] }))
      await page.goto('/')
      await browseTreeOnMobile(page)
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
      await browseTreeOnMobile(page)
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
      await browseTreeOnMobile(page)
      await expect(page.getByRole('treeitem', { name: 'Parent', exact: true })).toHaveAttribute('aria-expanded', 'false')
      await openWorkspaceSettings(page)
      await cloudboxRow(page).click()
      await page.locator('main section', { has: page.getByRole('heading', { name: 'Agents on this machine' }) }).getByRole('button', { name: 'Terminal', exact: true }).click()
      if (mobile) await backToWorkloads(page)
      await expect(page.getByRole('treeitem', { name: 'Parent', exact: true })).toHaveAttribute('aria-expanded', 'true')
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'true')
      await expect(page.getByRole('treeitem', { name: 'Other folder', exact: true })).toHaveAttribute('aria-expanded', 'false')
      await toggle(page)
      // The URL restores a selected workload (and reveals it); leave to Overview so this checks the stored preference.
      await page.getByRole('button', { name: 'Workspace overview' }).click()
      await page.reload()
      await browseTreeOnMobile(page)
      await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
    })
    for (const failure of ['getter', 'read', 'write']) {
      test(`storage ${failure} throws without breaking in-memory toggles`, async ({ page }) => {
        await page.addInitScript((failure) => {
          if (failure === 'getter') Object.defineProperty(window, 'localStorage', { get() { throw new Error('disabled') } })
          else Object.defineProperty(Storage.prototype, failure === 'read' ? 'getItem' : 'setItem', { value() { throw new Error('disabled') } })
        }, failure)
        await page.goto('/')
        await browseTreeOnMobile(page)
        await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
        await toggle(page)
        await expect(folder(page)).toHaveAttribute('aria-expanded', 'true')
        await toggle(page)
        await expect(folder(page)).toHaveAttribute('aria-expanded', 'false')
        await page.reload()
        await browseTreeOnMobile(page)
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

for (const width of [1280, 390]) for (const state of ['cleanup_failed', 'reserved_outcome_unknown']) {
  test(`reservation cleanup confirms, preserves draft and never auto-creates (${state}, ${width})`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 850 })
    let creates = 0, cleanups = 0
    await page.route('**/api/workspaces/ws-1/agents', route => {
      if (route.request().method() !== 'POST') return route.fallback()
      creates++
      return route.fulfill({ status: 409, json: { error: { code: 'spawn_failed', message: 'Startup could not be confirmed', details: {
        machine_id: 'srv-a', agent_id: 'ag_reserved', creation_state: state,
      } } } })
    })
    await page.route('**/api/workspaces/ws-1/agent-reservations/ag_reserved/cleanup', route => {
      cleanups++
      expect(route.request().method()).toBe('POST')
      expect(route.request().postDataJSON()).toEqual({})
      return ok(route, { agent_id: 'ag_reserved', deleted: true, already_deleted: false })
    })
    await page.goto('/')
    await browseTreeOnMobile(page)
    await page.getByRole('button', { name: 'Add to tree', exact: true }).click()
    await page.getByRole('menuitem', { name: 'Add agent', exact: true }).click()
    const create = page.getByRole('button', { name: 'Create terminal', exact: true })
    await page.getByRole('textbox', { name: 'Name', exact: true }).fill('Keep my draft')
    await create.click()
    await expect(create).toBeDisabled()
    await expect(page.getByText('ag_reserved', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Clean up reserved identity', exact: true }).click()
    const confirmation = page.getByRole('dialog').filter({ has: page.getByRole('heading', { name: 'Clean up reserved identity', exact: true }) })
    await confirmation.getByRole('button', { name: 'Cancel', exact: true }).click()
    expect(cleanups).toBe(0)
    await page.getByRole('button', { name: 'Clean up reserved identity', exact: true }).click()
    await page.screenshot({ path: info.outputPath(`reservation-${width}-${state}.png`) })
    await confirmation.getByRole('button', { name: 'Confirm cleanup', exact: true }).click()
    await expect(confirmation).toBeHidden()
    await expect(create).toBeEnabled()
    await expect(page.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue('Keep my draft')
    expect(cleanups).toBe(1)
    expect(creates).toBe(1)
    await expect(page.getByText(/Reservation removed/)).toBeVisible()
  })
}

for (const status of [403, 404, 409, 501, 503]) test(`reservation cleanup failure ${status} keeps identity and creation blocked`, async ({ page }) => {
  await page.route('**/api/workspaces/ws-1/agents', route => route.request().method() !== 'POST' ? route.fallback() : route.fulfill({ status: 409, json: { error: { message: 'Not confirmed', details: { machine_id: 'srv-a', agent_id: 'ag_reserved', creation_state: 'cleanup_failed' } } } }))
  let requests = 0
  await page.route('**/api/workspaces/ws-1/agent-reservations/ag_reserved/cleanup', route => { requests++; return route.fulfill({ status, json: { error: { code: status === 409 ? 'agent_reservation_observed' : 'cleanup_denied', message: 'Cleanup denied' } } }) })
  await page.goto('/')
  await page.getByRole('button', { name: 'Add to tree', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Add agent', exact: true }).click()
  await page.getByRole('textbox', { name: 'Name', exact: true }).fill('Keep refused cleanup draft')
  await page.getByRole('button', { name: 'Create terminal', exact: true }).click()
  await page.getByRole('button', { name: 'Clean up reserved identity', exact: true }).click()
  const confirmation = page.getByRole('dialog').filter({ has: page.getByRole('heading', { name: 'Clean up reserved identity', exact: true }) })
  await confirmation.getByRole('button', { name: 'Confirm cleanup', exact: true }).click()
  await expect(confirmation.getByRole('alert')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Create terminal', exact: true, includeHidden: true })).toBeDisabled()
  await expect(page.getByRole('textbox', { name: 'Name', exact: true, includeHidden: true })).toHaveValue('Keep refused cleanup draft')
  if (status === 409) await expect(confirmation.getByRole('alert')).toContainText('Core could not prove this workload is absent')
  if (status === 404) await expect(confirmation.getByRole('alert')).toContainText('does not confirm deletion or allow another Create')
  expect(requests).toBe(1)
})

for (const width of [1280, 390]) for (const theme of ['light', 'dark']) test(`missing command uses structured metadata and explicit retry state (${width}, ${theme})`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 850 })
  await page.addInitScript(value => localStorage.setItem('treer-theme', value), theme)
  let creates = 0
  await page.route('**/api/workspaces/ws-1/agents', route => {
    if (route.request().method() !== 'POST') return route.fallback()
    creates++
    return route.fulfill({ status: 422, json: { error: { code: 'command_not_found', message: 'Raw PATH must never appear', details: {
      command: 'claude', machine_id: 'srv-a', agent_id: 'ag_reserved', creation_state: creates === 1 ? 'identity_removed' : 'reserved_outcome_unknown',
    } } } })
  })
  await page.goto('/')
  await browseTreeOnMobile(page)
  await page.getByRole('button', { name: 'Add to tree', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Add agent', exact: true }).click()
  await page.getByRole('combobox', { name: 'Launch', exact: true }).click()
  await page.getByRole('option', { name: 'Custom command', exact: true }).click()
  await page.getByRole('textbox', { name: 'Command', exact: true }).fill('claude')
  await page.getByRole('textbox', { name: 'Name', exact: true }).fill('Preserved Claude')
  const submit = page.getByRole('button', { name: 'Create agent', exact: true })
  await submit.click()
  await expect(page.getByRole('alert')).toHaveText('claude is not installed on workstation.')
  await expect(page.getByRole('status')).toContainText('The reserved identity was removed')
  await expect(page.getByText('Raw PATH must never appear')).toHaveCount(0)
  await expect(submit).toBeEnabled()
  await expect(page.getByRole('button', { name: /^Install(\s|$)/ })).toHaveCount(0)
  expect(creates).toBe(1)
  await page.screenshot({ path: info.outputPath(`missing-command-${width}-${theme}.png`) })
  await submit.click()
  await expect(submit).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Clean up reserved identity', exact: true })).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue('Preserved Claude')
  expect(creates).toBe(2)
  const bounds = await page.getByRole('dialog').boundingBox()
  expect(bounds).not.toBeNull()
  expect(bounds!.y).toBeGreaterThanOrEqual(0)
  expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(850)
  await page.screenshot({ path: info.outputPath(`missing-command-unknown-${width}-${theme}.png`) })
})

// #123: a sealed, stateful provider API. No package manager or real install is
// executed; confirmation tokens below are synthetic fixture data only.
const providerRecipe = {
  provider_id: 'codex', recipe_id: 'codex-posix', recipe_version: 1, recipe_hash: 'a'.repeat(64), platform: 'linux-x86_64', binary_names: ['codex'], supported: true,
  official_docs: 'https://example.com/official', login_hint: 'Run codex explicitly to log in. Treer never handles provider credentials.', node_min: 22,
  lifecycle_scripts: { enabled: false }, package: { name: '@openai/codex', version: '1.2.3', integrity: 'sha512-fixture-only', registry: 'https://registry.npmjs.org/' },
  plan: [
    { step: 1, kind: 'command', purpose: 'pack', executable: '/opt/node/bin/npm', args: ['pack', '@openai/codex@1.2.3', '--userconfig=/private/jobs/codex/npmrc'] },
    { step: 2, kind: 'verify', purpose: 'integrity', algorithm: 'sha512', expected: 'sha512-fixture-only' },
    { step: 3, kind: 'command', purpose: 'install', executable: '/opt/node/bin/npm', args: ['install', '--global', '/private/jobs/codex/pack/package.tgz', '--ignore-scripts'] },
    { step: 4, kind: 'command', purpose: 'prefix', executable: '/opt/node/bin/npm', args: ['prefix', '--global', '--userconfig=/private/jobs/codex/npmrc'] },
    { step: 5, kind: 'resolve', purpose: 'check', rule: '<npm prefix --global>/bin/codex --version', binary: 'codex', args: ['--version'], prefix_from_step: 4 },
  ],
}
const providerConsent = { action: 'install', target_version: '1.2.3', observation_hash: 'd'.repeat(64), observation: { state: 'absent', executable: '/opt/global/bin/codex', found_in_prefix: false, probe: { exit_code: null, timed_out: false, cleanup_confirmed: true } } }
async function mockProviderApi(page: Page, options: { capability?: boolean; status?: string; prepareStatus?: number; oldMachine?: boolean; installStatus?: number; abortInstall?: boolean; output?: string; consent?: Record<string, unknown>; stateChanged?: boolean } = {}) {
  const requests: { path: string; body: unknown }[] = []
  let polls = 0
  await page.route('**/api/auth/config', route => ok(route, { capabilities: options.capability === false ? [] : ['machine.provider-cli-install.v2'] }))
  await page.route('**/api/workspaces/ws-1/machines/*/provider-clis/**', async route => {
    const path = new URL(route.request().url()).pathname
    if (route.request().method() === 'POST') requests.push({ path, body: route.request().postDataJSON() })
    if (options.oldMachine) return route.fulfill({ status: 501, json: { error: { code: 'provider_cli_unsupported', message: 'Old Controller' } } })
    if (path.endsWith('/recipes')) return ok(route, { schema_version: 1, machine_id: 'srv-a', platform: providerRecipe.platform, controller_source_sha: 'f'.repeat(40), table_hash: 'b'.repeat(64), recipes: [providerRecipe, {
      provider_id: 'claude', recipe_id: 'claude-posix', recipe_version: 1, recipe_hash: 'c'.repeat(64), platform: providerRecipe.platform, binary_names: ['claude'], supported: false,
      official_docs: 'https://example.com/official', login_hint: 'Manual login', unsupported_reason: 'lifecycle_review_pending',
    }] })
    if (path.endsWith('/prepare')) {
      if (options.prepareStatus) return route.fulfill({ status: options.prepareStatus, json: { error: { message: 'Do not echo a raw server secret' } } })
      return ok(route, { ...providerConsent, confirmation_token: 'fixture-confirmation-token', expires_at: new Date(Date.now() + 120000).toISOString(), machine_id: 'srv-a', table_hash: 'b'.repeat(64), recipe: providerRecipe, ...options.consent })
    }
    if (path.endsWith('/install')) {
      if (options.abortInstall) return route.abort('connectionreset')
      if (options.installStatus) return route.fulfill({ status: options.installStatus, json: { error: { message: 'Do not echo a raw server secret' } } })
      return ok(route, { operation_id: 'op-fixture', machine_id: 'srv-a', status: options.status === 'unknown' ? 'unknown' : 'running', poll_after_ms: 1000 })
    }
    if (path.includes('/operations/')) {
      polls++
      const status = options.stateChanged ? 'failed' : options.status ?? 'succeeded'
      return ok(route, { operation_id: 'op-fixture', machine_id: 'srv-a', recipe_id: providerRecipe.recipe_id, recipe_version: 1, recipe_hash: providerRecipe.recipe_hash,
        ...(options.stateChanged ? { error_code: 'provider_cli_state_changed', observed_before: { state: 'installed', current_version: '9.0.0', found_in_prefix: true, observation_hash: 'e'.repeat(64) }, steps: [] } : {}),
        status, step: options.stateChanged ? 0 : status === 'running' ? 3 : 5, elapsed_ms: 1000, exit_code: status === 'succeeded' ? 0 : status === 'failed' ? 7 : null, timed_out: status === 'timed_out',
        stdout: status === 'running' ? '' : options.output ?? '<img src=x onerror="window.providerXss=1"> fixture output', stderr: '', truncated: status === 'failed' })
    }
    if (path.endsWith('/check')) return ok(route, { installed: true, found_in_prefix: true, version: 'codex 1.2.3', executable: '/opt/global/bin/codex', exit_code: 0, timed_out: false, stdout: 'codex 1.2.3', stderr: '', truncated: false })
    return route.fulfill({ status: 404 })
  })
  return { requests, polls: () => polls }
}
async function openProviderMachine(page: Page) {
  await page.goto('/')
  await browseTreeOnMobile(page)
  await openWorkspaceSettings(page)
  await workstationRow(page).click()
}
for (const width of [1280, 390]) for (const theme of ['light', 'dark']) test(`provider installer explicit plan cancel success and text output (${width}, ${theme})`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 900 })
  await page.addInitScript(value => localStorage.setItem('treer-theme', value), theme)
  const backend = await mockProviderApi(page)
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await expect(dialog.getByText('Installation scripts have not been approved', { exact: false })).toBeVisible()
  await expect(dialog.getByRole('button', { name: 'Install claude', exact: true })).toHaveCount(0)
  await dialog.getByRole('button', { name: 'Install codex', exact: true }).click()
  await expect(dialog.getByRole('list', { name: 'Execution plan' })).toBeVisible()
  await expect(dialog.getByText('sha512: sha512-fixture-only', { exact: true })).toBeVisible()
  await expect(dialog.getByText('<npm prefix --global>/bin/codex --version', { exact: true })).toBeVisible()
  await page.screenshot({ path: info.outputPath(`provider-plan-${width}-${theme}.png`) })
  await dialog.getByRole('button', { name: 'Confirm and install', exact: true }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: info.outputPath(`provider-confirm-${width}-${theme}.png`) })
  await dialog.getByRole('button', { name: 'Cancel', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(0)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await dialog.getByRole('button', { name: 'Install codex', exact: true }).click()
  await dialog.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(dialog.getByRole('status')).toContainText('The version probe in the npm global prefix passed.')
  await dialog.getByText('Technical details and output', { exact: true }).click()
  await expect(dialog.getByText('<img src=x onerror="window.providerXss=1"> fixture output', { exact: true })).toBeVisible()
  expect(await page.evaluate(() => (window as unknown as Record<string, unknown>).providerXss)).toBeUndefined()
  await dialog.getByText('Technical details and output', { exact: true }).click()
  await dialog.getByRole('button', { name: 'Check installed version' }).click()
  await expect(dialog.getByRole('status')).toHaveText('Version check passed: codex 1.2.3')
  await expect(dialog.getByText('/opt/global/bin/codex', { exact: true })).toBeVisible()
  await expect(dialog.getByText(/This does not prove the CLI is on the Controller PATH/)).toHaveCount(1)
  await expect(dialog.getByText('Run codex explicitly to log in. Treer never handles provider credentials.', { exact: true })).toHaveCount(1)
  await expect(dialog.locator('details')).not.toHaveAttribute('open', '')
  expect(backend.requests.filter(item => item.path.endsWith('/install')).map(item => item.body)).toEqual([{ confirmation_token: 'fixture-confirmation-token' }])
  expect(backend.requests.filter(item => item.path.endsWith('/prepare'))[0].body).toEqual({ recipe_id: providerRecipe.recipe_id, recipe_version: 1, recipe_hash: providerRecipe.recipe_hash })
  expect(backend.requests.filter(item => item.path.endsWith('/check'))[0].body).toEqual({ recipe_id: providerRecipe.recipe_id, recipe_version: 1, recipe_hash: providerRecipe.recipe_hash })
  expect(await page.evaluate(() => JSON.stringify(localStorage).includes('fixture-confirmation-token') || location.href.includes('fixture-confirmation-token'))).toBe(false)
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: info.outputPath(`provider-result-${width}-${theme}.png`) })
})
for (const status of ['failed', 'timed_out', 'unknown']) test(`provider installer ${status} does not replay installation`, async ({ page }) => {
  const backend = await mockProviderApi(page, { status })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('button', { name: 'Install codex', exact: true }).click()
  await dialog.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(dialog.getByRole('button', { name: 'Check installed version' })).toBeVisible()
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(1)
  await expect(dialog.getByRole('button', { name: 'Confirm and install' })).toHaveCount(0)
  if (status === 'unknown') expect(backend.polls()).toBe(0)
})
for (const prepareStatus of [403, 409, 429, 501]) test(`provider installer prepare refusal ${prepareStatus} stays explicit`, async ({ page }) => {
  const backend = await mockProviderApi(page, { prepareStatus })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('button', { name: 'Install codex', exact: true }).click()
  await expect(dialog.getByRole('alert')).toBeVisible()
  await expect(dialog.getByText('Do not echo a raw server secret')).toHaveCount(0)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(0)
})
for (const options of [{ capability: false }, { oldMachine: true }]) test(`provider installer unavailable hides entry (${JSON.stringify(options)})`, async ({ page }) => {
  await mockProviderApi(page, options)
  await openProviderMachine(page)
  await expect(page.getByRole('heading', { name: 'workstation' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Install agent CLIs', exact: true })).toHaveCount(0)
})
test('provider installer transport failure cannot resend token', async ({ page }) => {
  const backend = await mockProviderApi(page, { abortInstall: true })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('button', { name: 'Install codex', exact: true }).click()
  await dialog.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(dialog.getByRole('status')).toContainText('Installation outcome is unknown')
  await expect(dialog.getByRole('button', { name: 'Confirm and install' })).toHaveCount(0)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(1)
})
test('provider installer only offers a proven missing CLI and preserves creation draft', async ({ page }) => {
  const backend = await mockProviderApi(page)
  let creates = 0
  await page.route('**/api/workspaces/ws-1/agents', route => {
    if (route.request().method() !== 'POST') return route.fallback()
    creates++
    return route.fulfill({ status: 422, json: { error: { code: 'command_not_found', message: 'Missing executable', details: {
      command: 'codex', machine_id: 'srv-a', agent_id: 'ag-reserved', creation_state: 'identity_removed',
    } } } })
  })
  await page.goto('/')
  await browseTreeOnMobile(page)
  await page.getByRole('button', { name: 'Add to tree', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Add agent', exact: true }).click()
  await page.getByRole('combobox', { name: 'Launch', exact: true }).click()
  await page.getByRole('option', { name: 'Custom command', exact: true }).click()
  await page.getByRole('textbox', { name: 'Command', exact: true }).fill('codex')
  await page.getByRole('textbox', { name: 'Name', exact: true }).fill('Retained install draft')
  await page.getByRole('button', { name: 'Create agent', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  const installer = page.getByRole('dialog').filter({ has: page.getByRole('heading', { name: 'Install agent CLIs', exact: true }) })
  await installer.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(installer.getByRole('status')).toContainText('The version probe')
  await installer.getByRole('button', { name: 'Close', exact: true }).first().click()
  await expect(page.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue('Retained install draft')
  expect(creates).toBe(1)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(1)
})

test('provider installer closing pauses polls and reopening resumes without another install', async ({ page }) => {
  const backend = await mockProviderApi(page, { status: 'running' })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('button', { name: 'Install codex', exact: true }).click()
  await dialog.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect.poll(backend.polls).toBeGreaterThanOrEqual(1)
  await dialog.getByRole('button', { name: 'Close', exact: true }).first().click()
  const stopped = backend.polls()
  // Wait longer than the one-second poll interval to prove observation stops.
  await page.waitForTimeout(1200)
  expect(backend.polls()).toBe(stopped)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await expect.poll(backend.polls).toBeGreaterThan(stopped)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(1)
})
test('provider installer unknown status refresh is read-only', async ({ page }) => {
  const backend = await mockProviderApi(page, { status: 'unknown' })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('button', { name: 'Install codex', exact: true }).click()
  await dialog.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(dialog.getByRole('button', { name: 'Refresh operation status' })).toBeVisible()
  expect(backend.polls()).toBe(0)
  await dialog.getByRole('button', { name: 'Refresh operation status' }).click()
  await expect.poll(backend.polls).toBe(1)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(1)
})
for (const state of ['reserved_outcome_unknown', 'cleanup_failed', 'identity_removed-other-code', 'identity_removed-unknown-command']) test(`provider installer failure gating ${state}`, async ({ page }) => {
  await mockProviderApi(page)
  await page.route('**/api/workspaces/ws-1/agents', route => {
    if (route.request().method() !== 'POST') return route.fallback()
    return route.fulfill({ status: 422, json: { error: { code: state === 'identity_removed-other-code' ? 'spawn_failed' : 'command_not_found', message: 'Fixed failure', details: {
      command: state === 'identity_removed-unknown-command' ? 'unreviewed-tool' : 'codex', machine_id: 'srv-a', agent_id: 'ag-reserved', creation_state: state.startsWith('identity_removed') ? 'identity_removed' : state,
    } } } })
  })
  await page.goto('/')
  await browseTreeOnMobile(page)
  await page.getByRole('button', { name: 'Add to tree', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Add agent', exact: true }).click()
  await page.getByRole('button', { name: 'Create terminal', exact: true }).click()
  await expect(page.getByRole('alert')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Install codex', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toHaveCount(0)
})
test('provider installer discards a late preparation after close', async ({ page }) => {
  const backend = await mockProviderApi(page)
  let release: (() => void) | undefined
  let started = false
  await page.route('**/provider-clis/prepare', async route => {
    started = true
    await new Promise<void>(resolve => { release = resolve })
    return ok(route, { confirmation_token: 'late-fixture-token', expires_at: new Date(Date.now() + 120000).toISOString(), machine_id: 'srv-a', table_hash: 'b'.repeat(64), recipe: providerRecipe })
  })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await expect.poll(() => started).toBe(true)
  await page.getByRole('dialog').getByRole('button', { name: 'Close', exact: true }).first().click()
  release!()
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Install codex', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toHaveCount(0)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(0)
})
test('provider installer confirmation is localized and keyboard reachable at 390px', async ({ page }, info) => {
  await page.setViewportSize({ width: 390, height: 900 })
  await mockProviderApi(page)
  await openProviderMachine(page)
  await page.evaluate(async () => {
    // @ts-expect-error Vite module served in browser.
    const { setLocale } = await import('/src/i18n/index.ts'); setLocale('zh')
  })
  const trigger = page.getByRole('button', { name: '安装 Agent CLI', exact: true })
  await trigger.focus(); await page.keyboard.press('Enter')
  await page.getByRole('button', { name: '安装 codex', exact: true }).focus(); await page.keyboard.press('Enter')
  await expect(page.getByRole('heading', { name: '确认安装 codex', exact: true })).toBeFocused()
  await expect(page.getByRole('list', { name: '执行计划' })).toBeVisible()
  await page.screenshot({ path: info.outputPath('provider-plan-390-zh.png') })
  await page.keyboard.press('Escape')
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await expect(trigger).toBeFocused()
})

test('provider installer expired confirmation cannot dispatch', async ({ page }) => {
  await page.clock.install()
  const backend = await mockProviderApi(page)
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  const confirm = page.getByRole('button', { name: 'Confirm and install', exact: true })
  await expect(confirm).toBeEnabled()
  await page.clock.fastForward(121000)
  await expect(confirm).toBeDisabled()
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(0)
})
test('provider installer rejects a mismatched prepared machine', async ({ page }) => {
  const backend = await mockProviderApi(page)
  await page.route('**/provider-clis/prepare', route => ok(route, { ...providerConsent, confirmation_token: 'foreign-fixture-token', expires_at: new Date(Date.now() + 120000).toISOString(), machine_id: 'srv-b', table_hash: 'b'.repeat(64), recipe: providerRecipe }))
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await expect(page.getByRole('dialog').getByRole('alert')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toHaveCount(0)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(0)
})

test('provider installer lets the server decide expiry when the browser clock is ahead', async ({ page }) => {
  await page.clock.install({ time: new Date('2099-01-01T00:00:00Z') })
  const backend = await mockProviderApi(page)
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toBeEnabled()
  await page.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(1)
})


test('provider installer escapes terminal and bidirectional controls in technical output', async ({ page }) => {
  await mockProviderApi(page, { output: '\u001b[31mred\u0007\u200b\u200f\u202a\u202e\u2066\u2069\ufeff\nplain\ttext' })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await page.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await page.getByText('Technical details and output', { exact: true }).click()
  await expect(page.getByRole('dialog').locator('pre')).toHaveText('\\u001b[31mred\\u0007\\u200b\\u200f\\u202a\\u202e\\u2066\\u2069\\ufeff\nplain\ttext')
})

const installedObservation = (version: string | null, unknown = false) => ({
  state: unknown ? 'unknown' : 'installed', current_version: version, version_text: version,
  executable: '/opt/global/bin/codex', found_in_prefix: true,
  probe: { exit_code: unknown ? 1 : 0, timed_out: false, cleanup_confirmed: true, error_code: unknown ? 'provider_cli_version_unparsed' : null },
})
for (const width of [1280, 390]) for (const theme of ['light', 'dark']) for (const locale of ['en', 'zh']) test(`provider consent downgrade (${width}, ${theme}, ${locale})`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 900 })
  await page.addInitScript(value => localStorage.setItem('treer-theme', value), theme)
  const backend = await mockProviderApi(page, { consent: { action: 'replace', version_relation: 'downgrade', observation: installedObservation('2.0.0') } })
  await openProviderMachine(page)
  if (locale === 'zh') await page.evaluate(async () => {
    // @ts-expect-error Vite browser module.
    const { setLocale } = await import('/src/i18n/index.ts'); setLocale('zh')
  })
  await page.getByRole('button', { name: locale === 'en' ? 'Install agent CLIs' : '安装 Agent CLI', exact: true }).click()
  await page.getByRole('button', { name: locale === 'en' ? 'Install codex' : '安装 codex', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await expect(dialog.getByRole('alert')).toContainText(locale === 'en' ? 'Downgrade' : '降级')
  await expect(dialog.getByText(/2\.0\.0/)).toBeVisible()
  const confirm = dialog.getByRole('button', { name: locale === 'en' ? 'Confirm and install' : '确认并安装', exact: true })
  await expect(confirm).toBeDisabled()
  await page.screenshot({ path: info.outputPath(`consent-${width}-${theme}-${locale}.png`) })
  await dialog.getByRole('checkbox').scrollIntoViewIfNeeded()
  await expect(dialog.getByRole('checkbox')).not.toBeChecked()
  await expect(dialog.getByText(locale === 'en' ? 'Tick the box to enable installation.' : '勾选确认后才能安装。')).toBeVisible()
  await page.screenshot({ path: info.outputPath(`consent-unchecked-${width}-${theme}-${locale}.png`) })
  await dialog.getByRole('checkbox').focus(); await page.keyboard.press('Space')
  await expect(confirm).toBeEnabled()
  await page.screenshot({ path: info.outputPath(`consent-ack-${width}-${theme}-${locale}.png`) })
  await confirm.click()
  await expect.poll(() => backend.requests.filter(item => item.path.endsWith('/install')).length).toBe(1)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))[0].body).toEqual({ confirmation_token: 'fixture-confirmation-token', acknowledged_relation: 'downgrade' })
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})
for (const relation of ['upgrade', 'unknown', 'incomparable']) test(`provider consent acknowledgement ${relation}`, async ({ page }) => {
  const backend = await mockProviderApi(page, { consent: { action: 'replace', version_relation: relation, observation: installedObservation(relation === 'unknown' ? null : '1.0.0', relation === 'unknown') } })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  const confirm = page.getByRole('button', { name: 'Confirm and install', exact: true })
  if (relation === 'upgrade') await expect(page.getByRole('checkbox')).toHaveCount(0)
  else { await expect(confirm).toBeDisabled(); await page.locator('label').filter({ has: page.getByRole('checkbox') }).click({ position: { x: 80, y: 20 } }); await expect(page.getByRole('checkbox')).toBeChecked() }
  await confirm.click()
  await expect.poll(() => backend.requests.filter(item => item.path.endsWith('/install')).length).toBe(1)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))[0].body).toEqual({ confirmation_token: 'fixture-confirmation-token', ...(relation === 'upgrade' ? {} : { acknowledged_relation: relation }) })
})
for (const width of [1280, 390]) test(`provider consent noop (${width})`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 900 })
  const backend = await mockProviderApi(page, { consent: { action: 'noop', version_relation: undefined, observation: installedObservation('1.2.3'), confirmation_token: undefined, expires_at: undefined } })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Already installed', exact: true })).toBeVisible()
  await expect(page.getByText('Current: installed, self-reported version 1.2.3')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toHaveCount(0)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(0)
  await page.screenshot({ path: info.outputPath(`consent-noop-${width}.png`) })
})
for (const consent of [{ action: 'invented' }, { action: 'noop', observation: installedObservation('1.2.3') }, { observation: { ...installedObservation(null), state: 'invented' } }, { action: 'replace', version_relation: 'downgrade' }]) test(`provider consent malformed ${JSON.stringify(consent)}`, async ({ page }) => {
  const backend = await mockProviderApi(page, { consent })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await expect(page.getByRole('dialog').getByRole('alert')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toHaveCount(0)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(0)
})
test('provider consent state changed requires a fresh plan without automatic replay', async ({ page }) => {
  const backend = await mockProviderApi(page, { stateChanged: true })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await page.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(page.getByRole('dialog').getByRole('alert')).toContainText('installation state changed')
  await expect(page.getByText('Current: installed, self-reported version 9.0.0')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Check installed version' })).toHaveCount(0)
  expect(backend.requests.filter(item => item.path.endsWith('/prepare'))).toHaveLength(1)
  await page.getByRole('button', { name: 'Review a new plan', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toBeVisible()
  expect(backend.requests.filter(item => item.path.endsWith('/prepare'))).toHaveLength(2)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(1)
})

test('provider consent cancelled acknowledgement does not survive a fresh preparation', async ({ page }) => {
  const backend = await mockProviderApi(page, { consent: { action: 'replace', version_relation: 'downgrade', observation: installedObservation('2.0.0') } })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await page.getByRole('checkbox').check()
  await page.getByRole('button', { name: 'Cancel', exact: true }).click()
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await expect(page.getByRole('checkbox')).not.toBeChecked()
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toBeDisabled()
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(0)
})

test('provider consent v1-only Proxy never offers unobserved installation', async ({ page }) => {
  const backend = await mockProviderApi(page)
  await page.route('**/api/auth/config', route => ok(route, { capabilities: ['machine.provider-cli-install.v1'] }))
  await openProviderMachine(page)
  await expect(page.getByRole('button', { name: 'Install agent CLIs', exact: true })).toHaveCount(0)
  expect(backend.requests).toHaveLength(0)
})

test('provider consent outdated Controller explains update without fallback', async ({ page }) => {
  const backend = await mockProviderApi(page)
  await page.route('**/provider-clis/prepare', route => route.fulfill({ status: 409, json: { error: { code: 'provider_cli_controller_outdated', message: 'must not echo raw server details' } } }))
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText(/update/i)
  await expect(page.getByRole('alert')).not.toContainText('raw server details')
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toHaveCount(0)
  expect(backend.requests.filter(item => item.path.endsWith('/install'))).toHaveLength(0)
})

for (const code of ['provider_cli_acknowledgement_required', 'provider_cli_acknowledgement_mismatch']) test(`provider consent ${code} asks for fresh consent without replay`, async ({ page }) => {
  await mockProviderApi(page, { consent: { action: 'replace', version_relation: 'downgrade', observation: installedObservation('2.0.0') } })
  let installs = 0
  await page.route('**/provider-clis/install', route => {
    installs++
    return route.fulfill({ status: 400, json: { error: { code, message: 'Do not echo raw server details' } } })
  })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install codex', exact: true }).click()
  await page.getByRole('checkbox').check()
  await page.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(page.getByRole('alert')).toHaveText('Installation was not accepted. Review a new plan and tick the required acknowledgement before confirming.')
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toHaveCount(0)
  expect(installs).toBe(1)
})

// #131: all installs below are fake HTTP responses; no npm or provider executes.
async function mockNativeProviderApi(page: Page, options: { capability?: boolean; failure?: string; malformed?: boolean; prepareError?: string } = {}) {
  const { nativeRecipe } = await import('./fixtures/provider-native')
  const backend = await mockProviderApi(page)
  await page.route('**/api/auth/config', route => ok(route, { capabilities: ['machine.provider-cli-install.v2', ...(options.capability === false ? [] : ['machine.provider-cli-plan.v2'])] }))
  await page.route('**/provider-clis/recipes', route => ok(route, { schema_version: 2, machine_id: 'srv-a', platform: nativeRecipe.platform, controller_source_sha: 'f'.repeat(40), table_hash: 'b'.repeat(64), recipes: [nativeRecipe, { ...providerRecipe, expected_version: providerRecipe.package.version }] }))
  await page.route('**/provider-clis/prepare', route => {
    if (options.prepareError) return route.fulfill({ status: 409, json: { error: { code: options.prepareError, message: 'raw server details must not appear' } } })
    const recipe = structuredClone(nativeRecipe)
    if (options.malformed) recipe.plan[7].expected_sha256 = 'bad'
    return ok(route, { ...providerConsent, target_version: nativeRecipe.expected_version, observation: { ...providerConsent.observation, node_arch: 'x64' }, confirmation_token: 'fixture-native-confirmation', expires_at: new Date(Date.now() + 120000).toISOString(), machine_id: 'srv-a', table_hash: 'b'.repeat(64), recipe })
  })
  await page.route('**/provider-clis/operations/*', route => ok(route, { operation_id: 'op-fixture', machine_id: 'srv-a', recipe_id: nativeRecipe.recipe_id, recipe_version: 2, recipe_hash: nativeRecipe.recipe_hash,
    status: options.failure ? 'failed' : 'succeeded', step: options.failure === 'provider_cli_binary_mismatch' ? 8 : 9, elapsed_ms: 1000, exit_code: options.failure ? null : 0, timed_out: false,
    stdout: '', stderr: '', truncated: false, ...(options.failure ? { error_code: options.failure, prefix_modified: true } : { prefix_modified: false }),
    verified_file: options.failure === 'provider_cli_binary_mismatch' ? { failure: 'native_content' } : { size: nativeRecipe.native_binary.size, sha256: nativeRecipe.native_binary.sha256 } }))
  return backend
}
for (const width of [1280, 390]) for (const theme of ['light', 'dark']) test(`native provider nine-step consent and verified receipt (${width}, ${theme})`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 900 })
  await page.addInitScript(value => localStorage.setItem('treer-theme', value), theme)
  const backend = await mockNativeProviderApi(page)
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('button', { name: 'Install claude', exact: true }).click()
  await expect(dialog.getByRole('list', { name: 'Execution plan' }).getByRole('listitem')).toHaveCount(9)
  await expect(dialog.getByRole('alert')).toContainText('reviewed postinstall')
  await expect(dialog.getByText('Required Node architecture: x64', { exact: true })).toBeVisible()
  await expect(dialog.getByText(/Version probe must report the exact pinned version/)).toBeVisible()
  await expect(dialog.getByText(/Transitive dependencies are not locked/)).toHaveCount(0)
  await page.screenshot({ path: info.outputPath(`native-consent-${width}-${theme}.png`) })
  await dialog.getByText('Step 8 · Verify installed binary and entry link', { exact: true }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: info.outputPath(`native-verification-${width}-${theme}.png`) })
  await dialog.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(dialog.getByRole('status')).toContainText('The version probe')
  await expect(dialog.getByText(/Installed file matched the release pin/)).toBeVisible()
  await expect(page.getByText('fixture-native-confirmation', { exact: true })).toHaveCount(0)
  expect(backend.requests.filter(r => r.path.endsWith('/install'))).toHaveLength(1)
  expect(backend.requests.find(r => r.path.endsWith('/install'))!.body).toEqual({ confirmation_token: 'fixture-native-confirmation' })
  expect(await page.evaluate(() => Object.values(localStorage).join(' '))).not.toContain('fixture-native-confirmation')
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: info.outputPath(`native-result-${width}-${theme}.png`) })
})
for (const failure of ['provider_cli_binary_mismatch', 'provider_cli_version_mismatch']) test(`native provider ${failure} warns about prefix and never probes or retries`, async ({ page }) => {
  const backend = await mockNativeProviderApi(page, { failure })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('button', { name: 'Install claude', exact: true }).click()
  await dialog.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(dialog.getByRole('alert').filter({ hasText: 'global npm prefix may have been modified' })).toBeVisible()
  await expect(dialog.getByRole('alert').filter({ hasText: failure.includes('binary') ? 'version probe was not run' : 'incompatible runtime (including musl)' })).toBeVisible()
  await expect(dialog.getByRole('button', { name: 'Check installed version', exact: true })).toHaveCount(0)
  await expect(dialog.getByRole('button', { name: 'Confirm and install', exact: true })).toHaveCount(0)
  expect(backend.requests.filter(r => r.path.endsWith('/install'))).toHaveLength(1)
  expect(backend.requests.filter(r => r.path.endsWith('/check'))).toHaveLength(0)
})
test('native provider is hidden without plan capability while legacy provider stays available', async ({ page }) => {
  await mockNativeProviderApi(page, { capability: false })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Install claude', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Install codex', exact: true })).toBeVisible()
})
for (const prepareError of ['provider_cli_node_arch_mismatch', 'provider_cli_space_insufficient', 'provider_cli_controller_outdated', 'malformed']) test(`native provider prepare ${prepareError} is explicit and cannot dispatch`, async ({ page }) => {
  const backend = await mockNativeProviderApi(page, prepareError === 'malformed' ? { malformed: true } : { prepareError })
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install claude', exact: true }).click()
  await expect(page.getByRole('dialog').getByRole('alert')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Confirm and install', exact: true })).toHaveCount(0)
  expect(backend.requests.filter(r => r.path.endsWith('/install'))).toHaveLength(0)
  await expect(page.getByText('raw server details must not appear')).toHaveCount(0)
})

test('native provider Chinese consent remains keyboard accessible at 390px', async ({ page }, info) => {
  await page.setViewportSize({ width: 390, height: 900 })
  await mockNativeProviderApi(page)
  await openProviderMachine(page)
  await page.evaluate(async () => {
    // @ts-expect-error Vite browser module.
    const { setLocale } = await import('/src/i18n/index.ts'); setLocale('zh')
  })
  const trigger = page.getByRole('button', { name: '安装 Agent CLI', exact: true })
  await trigger.focus(); await page.keyboard.press('Enter')
  await page.getByRole('button', { name: '安装 claude', exact: true }).focus(); await page.keyboard.press('Enter')
  await expect(page.getByRole('heading', { name: '确认安装 claude', exact: true })).toBeFocused()
  await expect(page.getByRole('list', { name: '执行计划' }).getByRole('listitem')).toHaveCount(9)
  await expect(page.getByRole('alert')).toContainText('经过审查的 postinstall')
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: info.outputPath('native-consent-390-zh.png') })
  await page.keyboard.press('Escape')
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await expect(trigger).toBeFocused()
})

for (const status of ['unknown', 'succeeded']) test(`native provider ${status} without verified evidence cannot probe`, async ({ page }) => {
  const backend = await mockNativeProviderApi(page)
  const { nativeRecipe } = await import('./fixtures/provider-native')
  await page.route('**/provider-clis/operations/*', route => ok(route, { operation_id: 'op-fixture', machine_id: 'srv-a', recipe_id: nativeRecipe.recipe_id, recipe_version: 2, recipe_hash: nativeRecipe.recipe_hash,
    status, step: 9, elapsed_ms: 100, exit_code: 0, timed_out: false, stdout: '', stderr: '', truncated: false }))
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install claude', exact: true }).click()
  await page.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Refresh operation status', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Check installed version', exact: true })).toHaveCount(0)
  await expect(page.getByText(/Installed file matched the release pin/)).toHaveCount(0)
  expect(backend.requests.filter(r => r.path.endsWith('/check'))).toHaveLength(0)
})

for (const [status, step, warning] of [['unknown', 9, true], ['timed_out', 6, true], ['timed_out', 5, false]] as const) test(`native provider ${status} step ${step} explains uncertain prefix without a server flag`, async ({ page }) => {
  const backend = await mockNativeProviderApi(page)
  const { nativeRecipe } = await import('./fixtures/provider-native')
  await page.route('**/provider-clis/operations/*', route => ok(route, { operation_id: 'op-fixture', machine_id: 'srv-a', recipe_id: nativeRecipe.recipe_id, recipe_version: 2, recipe_hash: nativeRecipe.recipe_hash,
    status, step, elapsed_ms: 100, exit_code: null, timed_out: status === 'timed_out', stdout: '', stderr: '', truncated: false }))
  await openProviderMachine(page)
  await page.getByRole('button', { name: 'Install agent CLIs', exact: true }).click()
  await page.getByRole('button', { name: 'Install claude', exact: true }).click()
  await page.getByRole('button', { name: 'Confirm and install', exact: true }).click()
  await expect(page.getByRole('status')).toContainText(status === 'unknown' ? 'unknown' : 'time limit')
  await expect(page.getByRole('alert').filter({ hasText: 'global npm prefix may have been modified' })).toHaveCount(warning ? 1 : 0)
  await expect(page.getByRole('button', { name: 'Check installed version', exact: true })).toHaveCount(0)
  expect(backend.requests.filter(r => r.path.endsWith('/install'))).toHaveLength(1)
  expect(backend.requests.filter(r => r.path.endsWith('/check'))).toHaveLength(0)
})

test.describe('resumable machine upload', () => {
  const mib = 1024 * 1024
  // Core generates upl_ + two Uuid::new_v4().simple() values (32 hex each).
  const coreUploadId = 'upl_0123456789ab4def8123456789abcdef0123456789ab4def8123456789abcdef'
  async function support(page: Page) {
    await page.route('**/api/auth/config', (route) => ok(route, { capabilities: ['machine.upload.v3'] }))
    await page.route('**/api/workspaces/ws-1/snapshot', (route) => ok(route, { ...snapshot, servers: snapshot.servers.map((machine) => ({ ...machine, capabilities: ['machine.upload.v3'] })) }))
  }
  async function open(page: Page, size = 3 * mib, name = 'payload.bin', limit = '128') {
    await page.goto('/')
    await browseTreeOnMobile(page)
    await page.getByRole('link', { name: /^api-server(\s|$)/ }).click()
    await tabAction(page, 'Upload file')
    const dialog = page.getByRole('dialog')
    await expect(dialog.getByText(`Maximum ${limit} MiB`, { exact: false })).toBeVisible()
    // Synthetic file created in-browser; avoid shipping a 128 MiB fixture through
    // Playwright or retaining large buffers in source/artifacts.
    await dialog.locator('input[type=file]').evaluate((input, { size, name }) => {
      const transfer = new DataTransfer()
      transfer.items.add(new File([new Blob([new Uint8Array(size)])], name))
      ;(input as HTMLInputElement).files = transfer.files
      input.dispatchEvent(new Event('change', { bubbles: true }))
      const original = File.prototype.arrayBuffer
      File.prototype.arrayBuffer = function () {
        if (this.size > 16 * 1024 * 1024) throw new Error('whole-file buffering is forbidden')
        return original.call(this)
      }
    }, { size, name })
    return dialog
  }
  function fixture(page: Page, options: { dropPart?: boolean; failures?: number; commitDrop?: boolean; commitHold?: Promise<void>; partHold?: Promise<void>; partHoldAfter?: number; abortPending?: boolean; denied?: boolean; beginDrop?: boolean; unreconciled?: boolean; rateLimit?: boolean; rateAfter?: string; abortBusy?: boolean; abortCommitted?: boolean; hashConflict?: boolean; sizeMismatch?: boolean; unknownFailure?: boolean; commitFailure?: boolean; uploadId?: unknown; abortFailure?: boolean; outcome?: 'unknown' | 'failed'; outcomeAtBegin?: boolean; outcomeHttpError?: boolean } = {}) {
    const uploadId = options.uploadId ?? coreUploadId
    const calls = { begin: [] as string[], offsets: [] as number[], lengths: [] as number[], hashes: [] as string[], statuses: 0, commits: 0, aborts: 0, abortedIds: [] as string[] }
    let size = 0, committed = 0, state = 'active', dropped = false, attempts = 0
    const receipt = () => ({ server_id: 'srv-a', path: 'repo/payload.bin', bytes_written: size, sha256: '0'.repeat(64), committed_at: new Date().toISOString() })
    const status = () => ({ upload_id: uploadId, size, committed_offset: committed, part_bytes_max: 8 * mib, state: options.outcome && (options.outcomeAtBegin || calls.commits) ? options.outcome : state, failure_code: options.outcome && (options.outcomeAtBegin || calls.commits) ? 'outcome_unknown' : undefined, reconciled: !options.unreconciled, cleanup_pending: options.abortPending ?? false, result: state === 'committed' ? receipt() : undefined })
    const ready = page.route('**/api/workspaces/ws-1/machines/srv-a/uploads**', async (route) => {
      const request = route.request(), method = request.method(), url = new URL(request.url())
      if (method === 'POST' && url.pathname.endsWith('/uploads')) {
        size = request.postDataJSON().size
        calls.begin.push(request.headers()['idempotency-key'])
        if (options.beginDrop && calls.begin.length === 1) return route.abort('connectionreset')
        return ok(route, status())
      }
      if (method === 'GET') {
        calls.statuses++
        const value = { ...status(), ...(options.sizeMismatch ? { size: size + 1 } : {}), ...(options.unknownFailure ? { state: 'failed', failure_code: 'untrusted server path /private/secret' } : {}), ...(options.commitFailure && calls.commits ? { state: 'failed', failure_code: 'machine_file_exists' } : {}) }
        return ok(route, url.pathname.endsWith('/uploads') ? { uploads: [value] } : value)
      }
      if (method === 'DELETE') {
        calls.aborts++; calls.abortedIds.push(decodeURIComponent(url.pathname.split('/').at(-1)!))
        if (options.abortFailure) return route.fulfill({ status: 503, json: { error: { code: 'controller_unavailable' } } })
        if (options.abortBusy && calls.aborts === 1) return route.fulfill({ status: 409, headers: { 'Retry-After': '1' }, json: { error: { code: 'upload_busy' } } })
        if (options.abortCommitted) { state = 'committed'; return route.fulfill({ status: 409, headers: { 'Retry-After': '1' }, json: { error: { code: 'upload_committed' } } }) }
        state = 'aborted'; return ok(route, status())
      }
      if (method === 'PUT') {
        const offset = Number(url.searchParams.get('offset')), data = request.postDataBuffer()!
        calls.offsets.push(offset); calls.lengths.push(data.length); calls.hashes.push(request.headers()['x-treer-part-sha256'])
        const { createHash } = await import('node:crypto')
        expect(calls.hashes.at(-1)).toBe(createHash('sha256').update(data).digest('hex'))
        expect(data.length).toBeLessThanOrEqual(8 * mib)
        if (options.rateLimit && attempts++ === 0) return route.fulfill({ status: 429, headers: { 'Retry-After': options.rateAfter ?? '1' }, json: { error: { code: 'machine_upload_busy' } } })
        if (options.hashConflict) return route.fulfill({ status: 409, json: { error: { code: 'upload_part_conflict' } } })
        if (options.denied) return route.fulfill({ status: 403, json: { error: { code: 'policy_denied' } } })
        if (attempts++ < (options.failures ?? 0)) return route.abort('connectionreset')
        if (calls.offsets.length > (options.partHoldAfter ?? 0)) await options.partHold
        committed = offset + data.length
        if (options.dropPart && !dropped) { dropped = true; return route.abort('connectionreset') }
        return ok(route, { size, committed_offset: committed })
      }
      if (method === 'POST' && url.pathname.endsWith('/commit')) {
        calls.commits++
        await options.commitHold
        if (options.outcomeHttpError) return route.fulfill({ status: 409, json: { error: { code: 'outcome_unknown' } } })
        if (options.outcome) return ok(route, status())
        if (options.commitFailure) return route.fulfill({ status: 409, json: { error: { code: 'machine_file_exists' } } })
        state = 'committed'
        if (options.commitDrop && calls.commits === 1) return route.abort('connectionreset')
        return ok(route, receipt())
      }
      return route.abort()
    })
    return { calls, ready }
  }
  test('exactly 128 MiB uses bounded raw parts and SHA-256, not whole-file base64', async ({ page }) => {
    test.setTimeout(60_000)
    await support(page)
    const { calls, ready } = fixture(page); await ready
    const dialog = await open(page, 128 * mib)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByText('Uploaded repo/payload.bin (128.0 MiB).')).toBeVisible({ timeout: 45_000 })
    expect(calls.lengths.reduce((a, b) => a + b, 0)).toBe(128 * mib)
    expect(calls.lengths[0]).toBe(mib)
    expect(calls.lengths.some((length) => length > mib)).toBe(true)
    expect(calls.begin).toHaveLength(1)
    expect(calls.commits).toBe(1)
    await expect(dialog.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '100')
  })
  test('the earlier 32-hex handle remains compatible', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { uploadId: 'upl_' + 'a'.repeat(32) }); await ready
    const dialog = await open(page, 0)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByText('Uploaded repo/payload.bin (0 B).')).toBeVisible()
    expect(calls.commits).toBe(1)
  })
  for (const [name, id, cleanup] of [
    ['short hex', 'upl_' + 'a'.repeat(31), true],
    ['long hex', 'upl_' + 'a'.repeat(65), true],
    ['uppercase hex', 'upl_' + 'A'.repeat(32), true],
    ['path', '../other-upload', false],
    ['URL', 'https://other.invalid/upload', false],
    ['oversized handle', 'a'.repeat(81), false],
    ['non-string', 12345, false],
  ] as const) test(`rejected begin handle cleans up only a safe Controller identifier (${name})`, async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { uploadId: id }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('alert')).toContainText('Upload paused or failed')
    expect(calls.aborts).toBe(cleanup ? 1 : 0)
    expect(calls.abortedIds).toEqual(cleanup ? [id] : [])
    expect(calls.offsets).toHaveLength(0)
    expect(calls.commits).toBe(0)
    await expect(dialog.getByText('Upload cancelled.', { exact: false })).toHaveCount(0)
  })
  test('rejected begin handle with failed cleanup never claims removal or sends bytes', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { uploadId: 'upl_' + 'A'.repeat(32), abortFailure: true }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('alert')).toContainText('Upload paused or failed')
    expect(calls.aborts).toBe(1)
    expect(calls.offsets).toHaveLength(0)
    expect(calls.commits).toBe(0)
    await expect(dialog.getByText('The temporary file was removed.', { exact: false })).toHaveCount(0)
  })
  for (const state of ['unknown', 'failed'] as const) for (const atBegin of [true, false]) test(`terminal ${state}/outcome_unknown stays uncertain (${atBegin ? 'begin replay' : 'commit'})`, async ({ page }, info) => {
    await support(page)
    const { calls, ready } = fixture(page, { outcome: state, outcomeAtBegin: atBegin }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('alert')).toHaveText('The upload result could not be confirmed. Check the file on the machine before uploading again.')
    await expect(dialog.getByRole('button', { name: 'Cancel upload' })).toHaveCount(0)
    await dialog.getByRole('button', { name: 'Check result' }).click()
    await expect(dialog.getByRole('alert')).toContainText('Check the file on the machine')
    expect(calls.commits).toBe(atBegin ? 0 : 1)
    expect(calls.aborts).toBe(0)
    if (atBegin) expect(calls.offsets).toHaveLength(0)
    await expect(dialog.getByText('Uploaded repo/payload.bin', { exact: false })).toHaveCount(0)
    if (state === 'unknown' && !atBegin) {
      await page.setViewportSize({ width: 390, height: 900 })
      await page.evaluate(async () => { const { setLocale } = await import('/src/i18n/index.ts'); setLocale('zh') })
      await expect(dialog.getByRole('alert')).toHaveText('无法确认上传结果。再次上传前，请先检查机器上的文件。')
      await page.screenshot({ path: info.outputPath('upload-terminal-unknown-390-zh.png') })
    }
  })
  test('an outcome_unknown HTTP refusal also preserves commit uncertainty', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { outcome: 'unknown', outcomeHttpError: true }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('alert')).toContainText('Check the file on the machine')
    await dialog.getByRole('button', { name: 'Check result' }).click()
    await expect(dialog.getByRole('alert')).toContainText('Check the file on the machine')
    expect(calls.commits).toBe(1)
    expect(calls.aborts).toBe(0)
  })
  test('128 MiB + 1 is rejected before begin', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page); await ready
    const dialog = await open(page, 128 * mib + 1)
    await expect(dialog.getByRole('button', { name: 'Upload', exact: true })).toBeDisabled()
    await expect(dialog.getByRole('alert')).toHaveText('Files must be 128 MiB or smaller.')
    expect(calls.begin).toHaveLength(0)
  })
  test('lost begin replays the same idempotency key; zero-byte file commits without parts', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { beginDrop: true }); await ready
    const dialog = await open(page, 0)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByText('Uploaded repo/payload.bin (0 B).')).toBeVisible()
    expect(calls.begin).toHaveLength(2)
    expect(calls.begin[0]).toMatch(/^[A-Za-z0-9_-]{32,128}$/)
    expect(calls.begin[1]).toBe(calls.begin[0])
    expect(calls.offsets).toHaveLength(0)
    await expect(dialog.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '100')
  })
  test('lost part response queries committed offset and continues without duplicate bytes', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { dropPart: true }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByText('Uploaded repo/payload.bin (3.00 MiB).')).toBeVisible()
    expect(calls.offsets.slice(0, 2)).toEqual([0, mib])
    expect(calls.statuses).toBeGreaterThanOrEqual(2)
  })
  test('transport retries are bounded, then Resume retains the same upload id', async ({ page }) => {
    test.setTimeout(30_000)
    await support(page)
    const { calls, ready } = fixture(page, { failures: 4 }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('button', { name: 'Resume upload' })).toBeVisible({ timeout: 15_000 })
    expect(calls.offsets).toHaveLength(4)
    await expect(dialog.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '0')
    await dialog.getByRole('button', { name: 'Resume upload' }).click()
    await expect(dialog.getByText('Uploaded repo/payload.bin (3.00 MiB).')).toBeVisible()
    expect(calls.begin).toHaveLength(1)
  })
  test('Policy refusal is not retried or downgraded', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { denied: true }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('alert')).toContainText('Upload is not permitted')
    expect(calls.offsets).toHaveLength(1)
    await expect(dialog.getByRole('button', { name: 'Resume upload' })).toHaveCount(0)
  })
  test('commit in flight disables cancellation; lost response recovers the receipt', async ({ page }) => {
    await support(page)
    let release!: () => void
    const hold = new Promise<void>((resolve) => { release = resolve })
    const { calls, ready } = fixture(page, { commitHold: hold, commitDrop: true }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByText('Finishing upload. Waiting for the machine to confirm.')).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Cancel upload' })).toHaveCount(0)
    await expect(dialog.getByRole('button', { name: 'Close', exact: true })).toBeDisabled()
    release()
    await expect(dialog.getByText('Uploaded repo/payload.bin (3.00 MiB).')).toBeVisible()
    expect(calls.commits).toBe(1)
    expect(calls.statuses).toBeGreaterThanOrEqual(2)
  })
  for (const pending of [false, true]) test(`cancel waits for server cleanup acknowledgement (pending=${pending})`, async ({ page }) => {
    await support(page)
    let release!: () => void
    const hold = new Promise<void>((resolve) => { release = resolve })
    const { calls, ready } = fixture(page, { partHold: hold, abortPending: pending }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect.poll(() => calls.offsets.length).toBe(1)
    await dialog.getByRole('button', { name: 'Cancel upload' }).click()
    await expect(dialog.getByText(pending ? 'Upload stopped. Temporary file cleanup is pending on the machine.' : 'Upload cancelled. The temporary file was removed.')).toBeVisible()
    expect(calls.aborts).toBe(1)
    expect(calls.commits).toBe(0)
    release()
  })
  test('rate limits wait and reconcile without starting a new upload', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { rateLimit: true }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByText('Waiting to reconnect or retry…')).toBeVisible()
    await expect(dialog.getByText('Uploaded repo/payload.bin (3.00 MiB).')).toBeVisible()
    expect(calls.offsets.slice(0, 2)).toEqual([0, 0])
    expect(calls.begin).toHaveLength(1)
  })
  test('an unreconciled offset never permits a part or commit', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { unreconciled: true }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('button', { name: 'Resume upload' })).toBeVisible()
    expect(calls.offsets).toHaveLength(0)
    expect(calls.commits).toBe(0)
  })
  test('conflicting part hash is not retried', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { hashConflict: true }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('alert')).toBeVisible()
    expect(calls.offsets).toHaveLength(1)
    await expect(dialog.getByRole('button', { name: 'Resume upload' })).toHaveCount(0)
  })
  for (const committed of [false, true]) test(`cancel resolves a server race (committed=${committed})`, async ({ page }) => {
    await support(page)
    let release!: () => void
    const hold = new Promise<void>((resolve) => { release = resolve })
    const { calls, ready } = fixture(page, { partHold: hold, abortBusy: !committed, abortCommitted: committed }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect.poll(() => calls.offsets.length).toBe(1)
    await dialog.getByRole('button', { name: 'Cancel upload' }).click()
    if (!committed) {
      await expect.poll(() => calls.aborts).toBe(1)
      await expect(dialog.getByRole('button', { name: 'Cancel upload' })).toHaveCount(0)
      await expect(dialog.getByText('Cancelling and removing the temporary file…')).toBeVisible()
    }
    await expect(dialog.getByText(committed ? 'Uploaded repo/payload.bin (3.00 MiB).' : 'Upload cancelled. The temporary file was removed.')).toBeVisible()
    expect(calls.aborts).toBe(committed ? 1 : 2)
    release()
  })
  test('workspace navigation disposes the upload and never sends its next part', async ({ page }) => {
    await support(page)
    let release!: () => void
    const hold = new Promise<void>((resolve) => { release = resolve })
    const { calls, ready } = fixture(page, { partHold: hold }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect.poll(() => calls.offsets.length).toBe(1)
    await page.evaluate(() => { history.pushState(null, '', '/orgs/org-2/workspaces/ws-2'); window.dispatchEvent(new PopStateEvent('popstate')) })
    await expect(dialog).toHaveCount(0)
    release()
    await expect(page.getByText('Uploaded repo/payload.bin', { exact: false })).toHaveCount(0)
    expect(calls.commits).toBe(0)
    expect(calls.aborts).toBe(0) // Never issue a mutation using the next scope's credentials.
    expect(calls.offsets).toHaveLength(1)
  })
  for (const width of [1360, 390]) for (const theme of ['light', 'dark']) for (const lang of ['en', 'zh']) test(`missing machine capability offers explicit compatibility (${width}, ${theme}, ${lang})`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 900 })
    await page.addInitScript((theme) => localStorage.setItem('treer-theme', theme), theme)
    await page.route('**/api/auth/config', (route) => ok(route, { capabilities: ['machine.upload.v3'] }))
    // Real canary regression: successful snapshot, no capabilities key at all.
    await page.route('**/api/workspaces/ws-1/snapshot', (route) => ok(route, snapshot))
    const { calls, ready } = fixture(page); await ready
    const legacy: { url: string; method: string; headers: Record<string, string>; body: { content_base64: string; directory: string; file_name: string; overwrite: boolean } }[] = []
    await page.route('**/api/workspaces/ws-1/machines/srv-a/files', (route) => {
      legacy.push({ url: route.request().url(), method: route.request().method(), headers: route.request().headers(), body: route.request().postDataJSON() })
      return ok(route, { path: 'repo/payload.bin', bytes_written: 16 * mib })
    })
    const dialog = await open(page, 16 * mib)
    await page.evaluate(async (locale) => { const { setLocale } = await import('/src/i18n/index.ts'); setLocale(locale as 'en' | 'zh') }, lang)
    const upload = dialog.getByRole('button', { name: lang === 'en' ? 'Upload' : '上传', exact: true })
    const compatibility = dialog.getByRole('button', { name: lang === 'en' ? 'Use compatibility upload (up to 16 MiB)' : '使用兼容上传（最大 16 MiB）', exact: true })
    await expect(upload).toBeDisabled()
    await expect(compatibility).toBeVisible()
    expect(legacy).toHaveLength(0); expect(calls.begin).toHaveLength(0)
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: info.outputPath(`upload-missing-capability-${width}-${theme}-${lang}.png`) })
    await compatibility.focus(); await page.keyboard.press('Enter')
    await expect(dialog.getByText(lang === 'en' ? 'Maximum 16 MiB' : '当前版本最大 16 MiB', { exact: false })).toBeVisible()
    await expect(upload).toBeEnabled()
    expect(legacy).toHaveLength(0) // Choosing compatibility does not start a transfer.
    await upload.click()
    await expect(dialog.getByText(lang === 'en' ? 'Uploaded repo/payload.bin' : '已上传 repo/payload.bin', { exact: false })).toBeVisible()
    expect(legacy).toHaveLength(1); expect(calls.begin).toHaveLength(0)
    expect(legacy[0].url).toMatch(/\/api\/workspaces\/ws-1\/machines\/srv-a\/files$/)
    expect(legacy[0].method).toBe('POST')
    expect(legacy[0].headers['idempotency-key']).toBeUndefined()
    expect(legacy[0].headers['x-treer-part-sha256']).toBeUndefined()
    expect(legacy[0].headers['content-type']).toContain('application/json')
    expect(Object.keys(legacy[0].body).sort()).toEqual(['content_base64', 'directory', 'file_name', 'overwrite'])
    expect(legacy[0].body).toMatchObject({ directory: '.', file_name: 'payload.bin', overwrite: false })
    expect(legacy[0].body.content_base64.length).toBe(4 * Math.ceil(16 * mib / 3))
  })
  test('missing machine capability compatibility cap and retry discover resumable support', async ({ page }) => {
    await page.route('**/api/auth/config', (route) => ok(route, { capabilities: ['machine.upload.v3'] }))
    await page.route('**/api/workspaces/ws-1/snapshot', (route) => ok(route, snapshot))
    const { calls, ready } = fixture(page); await ready
    const dialog = await open(page, 16 * mib + 1)
    await dialog.getByRole('button', { name: 'Use compatibility upload (up to 16 MiB)', exact: true }).click()
    await expect(dialog.getByRole('button', { name: 'Upload', exact: true })).toBeDisabled()
    await expect(dialog.getByRole('alert')).toContainText('needs a newer Proxy and Controller')
    expect(calls.begin).toHaveLength(0)
    await dialog.locator('form').getByRole('button', { name: 'Close', exact: true }).click()
    const reopened = await open(page)
    await support(page)
    await page.evaluate(() => window.dispatchEvent(new Event('focus')))
    await reopened.getByRole('button', { name: 'Retry check', exact: true }).click()
    await expect(reopened.getByRole('button', { name: 'Upload', exact: true })).toBeEnabled({ timeout: 15000 })
    await expect(reopened.getByRole('button', { name: 'Use compatibility upload (up to 16 MiB)', exact: true })).toHaveCount(0)
    await reopened.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect.poll(() => calls.commits).toBe(1)
  })
  test('compatibility accepts an empty file and is not remembered when the dialog reopens', async ({ page }) => {
    await page.route('**/api/auth/config', (route) => ok(route, { capabilities: ['machine.upload.v3'] }))
    await page.route('**/api/workspaces/ws-1/snapshot', (route) => ok(route, snapshot))
    let transfers = 0
    await page.route('**/api/workspaces/ws-1/machines/srv-a/files', (route) => {
      transfers++
      expect(route.request().postDataJSON()).toEqual({ directory: '.', file_name: 'empty.bin', overwrite: false, content_base64: '' })
      expect(route.request().headers()['idempotency-key']).toBeUndefined()
      expect(route.request().headers()['x-treer-part-sha256']).toBeUndefined()
      return ok(route, { path: 'empty.bin', bytes_written: 0 })
    })
    const dialog = await open(page, 0, 'empty.bin')
    await dialog.getByRole('button', { name: 'Use compatibility upload (up to 16 MiB)', exact: true }).click()
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByText('Uploaded empty.bin', { exact: false })).toBeVisible()
    expect(transfers).toBe(1)
    await dialog.locator('form').getByRole('button', { name: 'Close', exact: true }).click()
    const reopened = await open(page)
    await expect(reopened.getByRole('button', { name: 'Upload', exact: true })).toBeDisabled()
    await expect(reopened.getByRole('button', { name: 'Use compatibility upload (up to 16 MiB)', exact: true })).toBeVisible()
    expect(transfers).toBe(1)
  })
  test('an explicit machine capability array without v3 selects legacy directly', async ({ page }) => {
    await page.route('**/api/auth/config', (route) => ok(route, { capabilities: ['machine.upload.v3'] }))
    await page.route('**/api/workspaces/ws-1/snapshot', (route) => ok(route, { ...snapshot, servers: snapshot.servers.map((machine) => ({ ...machine, capabilities: [] })) }))
    const dialog = await open(page, 1, 'payload.bin', '16')
    await expect(dialog.getByRole('button', { name: 'Upload', exact: true })).toBeEnabled()
    await expect(dialog.getByRole('button', { name: 'Use compatibility upload (up to 16 MiB)', exact: true })).toHaveCount(0)
  })
  for (const status of [404, 503]) test(`compatibility is not offered after a failed inventory retry (${status})`, async ({ page }) => {
    await page.route('**/api/auth/config', (route) => ok(route, { capabilities: ['machine.upload.v3'] }))
    await page.route('**/api/workspaces/ws-1/snapshot', (route) => ok(route, snapshot))
    const dialog = await open(page)
    await expect(dialog.getByRole('button', { name: 'Use compatibility upload (up to 16 MiB)', exact: true })).toBeVisible()
    await page.route('**/api/workspaces/ws-1/snapshot', (route) => route.fulfill({ status, json: { error: { code: 'unavailable' } } }))
    await dialog.getByRole('button', { name: 'Retry check', exact: true }).click()
    await expect(dialog.getByText('Cannot check upload support. Retry before uploading.')).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Use compatibility upload (up to 16 MiB)', exact: true })).toHaveCount(0)
    await expect(dialog.getByRole('button', { name: 'Upload', exact: true })).toBeDisabled()
  })
  for (const status of [403, 503]) test(`failed capability discovery never silently falls back to base64 (${status})`, async ({ page }) => {
    await support(page)
    await page.route('**/api/auth/config', (route) => route.fulfill({ status, json: { error: { code: 'unavailable' } } }))
    const dialog = await open(page)
    await expect(dialog.getByText('Cannot check upload support. Retry before uploading.')).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Upload', exact: true })).toBeDisabled()
    await expect(dialog.getByText('Maximum 16 MiB', { exact: false })).toHaveCount(0)
    await expect(dialog.getByRole('button', { name: 'Use compatibility upload (up to 16 MiB)', exact: true })).toHaveCount(0)
  })
  test('missing secure hashing is rejected before begin', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page); await ready
    await page.addInitScript(() => Object.defineProperty(crypto, 'subtle', { value: undefined }))
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('alert')).toContainText('Secure hashing is unavailable')
    expect(calls.begin).toHaveLength(0)
  })
  for (const width of [1360, 390]) for (const theme of ['light', 'dark']) for (const lang of ['en', 'zh']) test(`upload progress fits (${width}, ${theme}, ${lang})`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 900 })
    await page.addInitScript(({ theme, lang }) => { localStorage.setItem('treer-theme', theme); localStorage.setItem('treer-lang', lang) }, { theme, lang })
    await support(page)
    let release!: () => void
    const hold = new Promise<void>((resolve) => { release = resolve })
    const { calls, ready } = fixture(page, { partHold: hold, partHoldAfter: 1 }); await ready
    // Open with English names, then switch the dictionary without reloading.
    await page.addInitScript(() => localStorage.setItem('treer-lang', 'en'))
    const dialog = await open(page)
    await page.evaluate(async (locale) => { const { setLocale } = await import('/src/i18n/index.ts'); setLocale(locale as 'en' | 'zh') }, lang)
    await dialog.getByRole('button', { name: lang === 'en' ? 'Upload' : '上传', exact: true }).click()
    await expect.poll(() => calls.offsets.length).toBe(2)
    await expect(dialog.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '33')
    await expect(dialog.getByRole('button', { name: lang === 'en' ? 'Cancel upload' : '取消上传' })).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: info.outputPath(`upload-${width}-${theme}-${lang}.png`) })
    await page.keyboard.press('Escape')
    await expect(dialog).toBeVisible()
    await dialog.getByRole('button', { name: lang === 'en' ? 'Cancel upload' : '取消上传' }).focus()
    await page.keyboard.press('Enter')
    await expect(dialog.getByText(lang === 'en' ? 'Upload cancelled. The temporary file was removed.' : '上传已取消，临时文件已移除。')).toBeVisible()
    release()
  })
  for (const kind of ['sizeMismatch', 'unknownFailure'] as const) test(`invalid status fails closed with a fixed message (${kind})`, async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { [kind]: true }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('alert')).toHaveText('Upload paused or failed. Retry to resume if available, or cancel before starting again.')
    await expect(dialog.getByText('untrusted server path', { exact: false })).toHaveCount(0)
    expect(calls.offsets).toHaveLength(0)
    expect(calls.commits).toBe(0)
  })
  test('existing destination after commit refusal never implies successful publication', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page, { commitFailure: true }); await ready
    const dialog = await open(page)
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('alert')).toContainText('result is not yet confirmed')
    await dialog.getByRole('button', { name: 'Check result' }).click()
    await expect(dialog.getByRole('alert')).toContainText('A file with this name already exists')
    await expect(dialog.getByText('Uploaded repo/payload.bin', { exact: false })).toHaveCount(0)
    expect(calls.commits).toBe(1)
  })
  for (const width of [1360, 390]) for (const lang of width === 390 ? ['en', 'zh'] : ['en']) for (const scenario of ['waiting', 'unknown', 'pending', 'too-large', 'success', 'failure']) test(`upload state ${scenario} (${width}, ${lang})`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 900 })
    await support(page)
    let release!: () => void
    const hold = new Promise<void>((resolve) => { release = resolve })
    const { calls, ready } = fixture(page, { rateLimit: scenario === 'waiting', rateAfter: '10', commitFailure: scenario === 'unknown', abortPending: scenario === 'pending', partHold: scenario === 'pending' ? hold : undefined, denied: scenario === 'failure' }); await ready
    const dialog = await open(page, scenario === 'too-large' ? 128 * mib + 1 : 3 * mib)
    await page.evaluate(async (locale) => { const { setLocale } = await import('/src/i18n/index.ts'); setLocale(locale as 'en' | 'zh') }, lang)
    if (scenario !== 'too-large') await dialog.getByRole('button', { name: lang === 'en' ? 'Upload' : '上传', exact: true }).click()
    if (scenario === 'pending') {
      await expect.poll(() => calls.offsets.length).toBe(1)
      await dialog.getByRole('button', { name: lang === 'en' ? 'Cancel upload' : '取消上传' }).click()
    }
    const copy: Record<string, [string, string]> = {
      waiting: ['Waiting to reconnect or retry…', '正在等待重新连接或重试…'],
      unknown: ['The upload result is not yet confirmed.', '上传结果尚未确认。'],
      pending: ['Upload stopped. Temporary file cleanup is pending on the machine.', '上传已停止，机器上的临时文件待清理。'],
      'too-large': ['Files must be 128 MiB or smaller.', '文件不能超过 128 MiB。'],
      success: ['Uploaded repo/payload.bin (3.00 MiB).', '已上传 repo/payload.bin(3.00 MiB)。'],
      failure: ['Upload is not permitted.', '没有上传权限。'],
    }
    await expect(dialog.getByText(copy[scenario][lang === 'en' ? 0 : 1], { exact: false })).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    await page.screenshot({ path: info.outputPath(`upload-${scenario}-${width}-${lang}.png`) })
    release()
  })
  test('legacy files over 16 MiB are rejected before transfer', async ({ page }) => {
    await page.route('**/api/auth/config', (route) => ok(route, {}))
    const { calls, ready } = fixture(page); await ready
    const dialog = await open(page, 17 * mib, 'payload.bin', '16')
    await expect(dialog.getByRole('button', { name: 'Upload', exact: true })).toBeDisabled()
    await expect(dialog.getByRole('alert')).toContainText('needs a newer Proxy and Controller')
    expect(calls.begin).toHaveLength(0)
  })
  test('control characters in a file name are rejected before begin', async ({ page }) => {
    await support(page)
    const { calls, ready } = fixture(page); await ready
    const dialog = await open(page, 0, 'bad\u0001.bin')
    await dialog.getByRole('button', { name: 'Upload', exact: true }).click()
    await expect(dialog.getByRole('alert')).toContainText('without slashes or control characters')
    expect(calls.begin).toHaveLength(0)
  })
})


test("mobile explicit Browse tree survives delayed deep-link inventory initialization", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  let resume!: () => void
  const held = new Promise<void>((resolve) => { resume = resolve })
  await page.route("**/api/workspaces/ws-1/apps", async (route) => {
    await held
    return ok(route, { apps: [] })
  })
  await page.goto("/orgs/org-1/workspaces/ws-1/workloads/ag-1")
  const tree = page.locator("#tree-sidebar")
  await page.getByRole("button", { name: "Browse tree", exact: true }).click()
  await expect(tree).toBeVisible()
  const loaded = page.waitForResponse((response) => response.url().endsWith("/ws-1/apps"))
  resume()
  await loaded
  await expect(page.getByRole("tabpanel", { name: "/api-server", exact: true, includeHidden: true })).toBeAttached()
  await expect(tree).toBeVisible()
  await tree.getByRole("treeitem", { name: "api-server", exact: true }).click()
  await expect(page.getByRole("tabpanel", { name: "/api-server", exact: true })).toBeVisible()
})

// #223: the Tree entry shares the real workspace bootstrap/dialog flow.
for (const mobile of [false, true]) for (const enrolled of [false, true]) test(`Tree Add machine opens shared workspace dialog (${enrolled ? 'enrolled' : 'empty'}, ${mobile ? '390' : 'desktop'})`, async ({ page }, info) => {
  await page.setViewportSize({ width: mobile ? 390 : 1360, height: 844 })
  if (!enrolled) await page.route('**/api/workspaces/ws-1/snapshot', route => ok(route, { ...snapshot, servers: [], agents: [] }))
  const requests: { path: string; body: unknown }[] = []
  await page.route('**/api/workspaces/*/bootstrap', route => {
    requests.push({ path: new URL(route.request().url()).pathname, body: route.request().postDataJSON() })
    return ok(route, { install_command: 'echo synthetic install', connect_command: `echo connect ${new URL(route.request().url()).pathname}` })
  })
  await page.goto('/')
  await browseTreeOnMobile(page)
  const trigger = page.getByRole('button', { name: 'Add to tree', exact: true })
  await expect(page.getByRole('tree', { name: 'Workspace workloads' })).toHaveAttribute('aria-busy', 'false')
  await expect.poll(async () => { await trigger.focus(); return trigger.evaluate(e => e === document.activeElement) }).toBe(true)
  await trigger.press('Enter')
  await expect(page.getByRole('menuitem', { name: 'Add folder', exact: true })).toBeVisible()
  await expect(page.getByRole('menuitem', { name: enrolled ? 'Add agent' : 'Set up a machine before creating an agent', exact: true })).toBeVisible()
  const add = page.getByRole('menuitem', { name: 'Add machine', exact: true })
  await expect(add).toBeEnabled()
  if (mobile) expect((await add.boundingBox())!.height).toBeGreaterThanOrEqual(44)
  await page.screenshot({ path: info.outputPath('tree-add-machine-menu.png') })
  await add.focus(); await page.keyboard.press('Enter')
  const setup = page.getByRole('dialog', { name: 'Add machine', exact: true })
  await expect(setup).toBeVisible(); await expect(page.getByRole('menu')).toHaveCount(0)
  await expect(setup.locator('textarea').last()).toHaveValue('echo connect /api/workspaces/ws-1/bootstrap')
  await expect.poll(() => setup.evaluate(e => e.contains(document.activeElement))).toBe(true)
  expect(requests).toEqual([{ path: '/api/workspaces/ws-1/bootstrap', body: {} }])
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({ path: info.outputPath('tree-add-machine-dialog.png') })
  await page.keyboard.press('Escape'); await expect(setup).toHaveCount(0)
  await expect(trigger).toBeFocused()
  // The original Add folder action still opens its editor after the dialog.
  await trigger.click(); await page.getByRole('menuitem', { name: 'Add folder', exact: true }).click()
  await expect(page.getByRole('dialog')).toBeVisible()
})
test('Tree Add machine follows current workspace and has no selected-folder payload', async ({ page }) => {
  const requests: string[] = []
  await page.route('**/api/workspaces/*/bootstrap', route => { requests.push(new URL(route.request().url()).pathname); expect(route.request().postDataJSON()).toEqual({}); return ok(route, { install_command: 'echo synthetic', connect_command: 'echo workspace two' }) })
  await page.goto('/orgs/org-2/workspaces/ws-2')
  await page.getByRole('button', { name: 'Add to tree', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Add machine', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Add machine', exact: true })).toBeVisible()
  expect(requests).toEqual(['/api/workspaces/ws-2/bootstrap'])
})

test('Tree Add machine bootstrap refusal clears the Tree focus target before settings opens', async ({ page }) => {
  let calls = 0
  await page.route('**/api/workspaces/ws-1/bootstrap', route => {
    calls++
    return calls === 1 ? route.fulfill({ status: 403, json: { error: 'synthetic bootstrap refusal' } }) : ok(route, { install_command: 'echo synthetic', connect_command: 'echo workspace one' })
  })
  await page.goto('/')
  const plus = page.getByRole('button', { name: 'Add to tree', exact: true })
  await plus.click(); await page.getByRole('menuitem', { name: 'Add machine', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('HTTP 403')
  await expect(page.getByRole('dialog', { name: 'Add machine', exact: true })).toHaveCount(0)
  await openWorkspaceSettings(page)
  await page.locator('[data-tour="add-machine"]').getByRole('button', { name: 'Add', exact: true }).click()
  const setup = page.getByRole('dialog', { name: 'Add machine', exact: true })
  await expect(setup).toBeVisible()
  await page.keyboard.press('Escape'); await expect(setup).toHaveCount(0)
  // Wait through deferred focus restoration before checking the failed entry is gone.
  await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))))
  await expect(plus).not.toBeFocused()
  expect(calls).toBe(2)
})
