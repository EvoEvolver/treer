import { expect, type Page, type WebSocketRoute } from '@playwright/test'
import { mockTreeMapApi, mapSnapshot } from './treemap'
export const names = ['Terminal', 'Alpha', 'Beta', 'Gamma', 'Delta', 'Epsilon', 'Zeta', 'Archive']
export async function fixture(page: Page, origins: Record<string, string> = {}) {
  await mockTreeMapApi(page)
  const agents = names.map((name, i) => ({ agent_id: `tab-${i}`, name, kind: i === 7 ? 'app' : 'command', server_id: 'local', status: i === 7 ? 'idle' : i === 2 ? 'blocked' : 'working', path: `/${name}`, ...(i && i !== 7 ? { interface: { origin_url: origins[`tab-${i}`], protocol: 'treer.agent-interface/v1', instance_id: `ui-${i}`, ui_path: '/', port: 8080, capabilities: [], registered_at: '2026-10-15T00:00:00Z' } } : {}) }))
  const app = { app_id: 'app-archive', runtime_agent_id: 'tab-7', name: 'Archive', server_id: 'local', status: 'running', hostname: 'archive.test', public_url: null }
  const build = { version: 'test', git_commit: 'test' }
  let snapshot = { ...mapSnapshot, servers: mapSnapshot.servers.map((machine) => ({ ...machine, controller_build: build, host_build: build })), agents }, apps = [app]
  await page.route('**/snapshot', (route) => route.fulfill({ json: snapshot }))
  await page.route('**/apps', (route) => route.fulfill({ json: { apps } }))
  await page.route('**/workload-tree', (route) => route.fulfill({ json: { folders: [], placements: [] } }))
  await page.route('**/groups', (route) => route.fulfill({ json: { groups: [] } }))
  await page.route('**/policy-provider', (route) => route.fulfill({ status: 404, json: {} }))
  await page.route('**/api/workspaces/**/ui/**', (route) => route.fulfill({ contentType: 'text/html', body: '<!doctype html><style>body{font:14px system-ui;padding:20px;background:#faf9f6;color:#24221e}body.dark{background:#22211e;color:#e7e5e0}textarea{display:block;box-sizing:border-box;width:100%;max-width:600px;height:100px;padding:12px;background:transparent;color:inherit;border:1px solid #888;border-radius:8px}main{height:1800px}</style><h1>Retained interface</h1><label>Draft<textarea aria-label="Draft"></textarea></label><main>Scroll stays here</main><script>document.body.classList.toggle("dark",new URLSearchParams(location.search).get("theme")==="dark");window.bootId=Math.random();</script>' }))
  let events: WebSocketRoute | undefined
  const connections: string[] = [], closed: string[] = [], inputs: string[] = []
  const terminals = new Map<string, WebSocketRoute>()
  await page.routeWebSocket(/\/events$/, (socket) => { events = socket })
  await page.routeWebSocket(/\/terminal(?:\?.*)?$/, (socket) => {
    const id = new URL(socket.url()).pathname.split('/').at(-2)!
    connections.push(id)
    terminals.set(id, socket)
    socket.onMessage((data) => { if (Buffer.isBuffer(data)) inputs.push(data.toString()) })
    socket.onClose(() => closed.push(id))
    socket.send(JSON.stringify({ type: 'ready', stream_epoch: 'test', revision: 1, replay_chunks: 0 }))
    socket.send(Buffer.from('retained terminal output\r\n'))
  })
  return { snapshot: () => snapshot, event: (event: string, data: unknown) => events!.send(JSON.stringify({ event, data })), connections, closed, inputs, output: (id: string, text: string) => terminals.get(id)!.send(Buffer.from(text)), remove: (id: string) => { snapshot = { ...snapshot, revision: snapshot.revision + 1, agents: snapshot.agents.filter((agent) => agent.agent_id !== id) }; apps = apps.filter((entry) => entry.runtime_agent_id !== id); events!.send(JSON.stringify({ event: 'workspace.snapshot', data: snapshot })) } }
}
export async function gotoWorkspace(page: Page) {
  await page.goto('/')
  // Auth renders the tab bar before workspace discovery. Discovery changes its
  // scope/key and closes a transient mobile switcher; wait for real inventory.
  await expect(page.getByRole('link', { name: 'Open /Terminal', exact: true })).toBeVisible()
}
export async function switchTab(page: Page, name: string, mobile = false) {
  if (mobile) {
    await page.getByRole('button', { name: /^Show all tabs/ }).click()
    await page.getByRole('button', { name: `Open tab ${name === 'Overview' ? name : '/'+name}`, exact: true }).click()
  } else await page.getByRole('tab', { name: `Tab ${name === 'Overview' ? name : '/'+name}`, exact: true }).click()
}
export async function open(page: Page, name: string, mobile = false) {
  await switchTab(page, 'Overview', mobile)
  await page.getByRole('link', { name: `Open /${name}`, exact: true }).click()
}
