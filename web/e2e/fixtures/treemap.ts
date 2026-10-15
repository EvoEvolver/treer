import type { Page } from '@playwright/test'

const user = { user_id: 'map-user', email: 'map@example.test', preferred_name: 'Map reviewer' }
const workspace = { workspace_id: 'map-workspace', organization_id: 'map-org', name: 'Studio' }
const machines = [
  { server_id: 'local', name: 'workstation', hostname: 'local.test', status: 'online', root: '/synthetic' },
  { server_id: 'remote', name: 'build-node', hostname: 'remote.test', status: 'offline', root: '/synthetic' },
]
const folders = [
  { folder_id: 'research', name: 'Research', parent_id: null },
  { folder_id: 'experiments', name: 'Experiments', parent_id: 'research' },
  { folder_id: 'product', name: 'Product', parent_id: null },
  { folder_id: 'empty', name: 'Ideas', parent_id: null },
]
const agents = [
  { agent_id: 'reviewer-a', name: 'reviewer', kind: 'codex', status: 'working', server_id: 'local', path: '/Research/reviewer' },
  { agent_id: 'researcher', name: 'literature', kind: 'claude', status: 'idle', server_id: 'local' },
  { agent_id: 'lab', name: 'prototype', kind: 'command', status: 'idle', server_id: 'local' },
  { agent_id: 'reviewer-b', name: 'reviewer', kind: 'claude', status: 'idle', server_id: 'remote', path: '/Product/reviewer' },
  { agent_id: 'designer', name: 'design', kind: 'codex', status: 'working', server_id: 'local' },
  { agent_id: 'checks', name: 'checks', kind: 'command', status: 'idle', server_id: 'local' },
  { agent_id: 'leader', name: 'coordinator', kind: 'claude', status: 'idle', server_id: 'local' },
]
const tree = { folders, placements: [
  { agent_id: 'reviewer-a', folder_id: 'research', position: 0 },
  { agent_id: 'researcher', folder_id: 'research', position: 1 },
  { agent_id: 'lab', folder_id: 'experiments', position: 0 },
  { agent_id: 'reviewer-b', folder_id: 'product', position: 0 },
  { agent_id: 'designer', folder_id: 'product', position: 1 },
  { agent_id: 'checks', folder_id: 'product', position: 2 },
] }
export const mapSnapshot = { revision: 1, workspace, servers: machines, agents }

export async function mockTreeMapApi(page: Page) {
  await page.routeWebSocket(/\/events$/, () => {})
  await page.routeWebSocket(/\/terminal(?:\?.*)?$/, () => {})
  await page.route('**/api/**', (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === "/api/invitations/pending") return route.fulfill({ status: 204 })
    let body: unknown = {}
    if (path.endsWith('/auth/me')) body = user
    else if (path.endsWith('/organizations')) body = { organizations: [{ organization_id: 'map-org', name: 'Studio', role: 'owner' }] }
    else if (path.endsWith('/workspaces')) body = { workspaces: [workspace] }
    else if (path.endsWith('/snapshot')) body = mapSnapshot
    else if (path.endsWith('/workload-tree')) body = tree
    else if (path.endsWith('/apps')) body = { apps: [] }
    else if (path.endsWith('/members')) body = { members: [] }
    else if (path.endsWith('/launch-profiles')) body = { profiles: [] }
    else if (path.endsWith('/access')) body = { access: { current_role: 'owner', members: [], groups: [] } }
    else if (path.endsWith('/virtual-hosts')) body = { hosts: [] }
    else if (path.endsWith('/services')) body = { services: [] }
    else if (path.endsWith('/traffic')) body = { traffic: [] }
    return route.fulfill({ contentType: 'application/json', body: JSON.stringify(body) })
  })
}


export async function mockDeepTreeMapApi(page: Page, large: boolean | "medium", depth = 4) {
  await mockTreeMapApi(page)
  const nestedFolders = Array.from({ length: depth }, (_, index) => ({
    folder_id: `level-${index}`, name: ['Team', 'Research', 'Experiments', 'Lab'][index] ?? `Level-${index}`, parent_id: index ? `level-${index - 1}` : null,
  }))
  const nestedAgents = Array.from({ length: large === "medium" ? 39 : large ? 299 : 4 }, (_, index) => ({
    agent_id: `deep-${index}`, name: `agent-${String(index + 1).padStart(2, '0')}`, kind: index % 2 ? 'codex' : 'command',
    status: index % 3 ? 'idle' : 'working', server_id: index % 11 === 10 ? 'remote' : 'local',
  }))
  await page.route('**/snapshot', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({
    ...mapSnapshot, agents: [{ agent_id: 'app-runtime', name: 'dashboard', kind: 'app', status: 'running', server_id: 'local' }, ...nestedAgents],
  }) }))
  await page.route('**/workload-tree', (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify({
    folders: large === "medium" ? [...nestedFolders, { folder_id: "operations", name: "Operations", parent_id: null }, { folder_id: "product", name: "Product", parent_id: null }] : nestedFolders,
    placements: nestedAgents.map((agent, position) => ({ agent_id: agent.agent_id, folder_id: large === "medium" && position >= 6 ? position < 13 ? "operations" : "product" : `level-${depth - 1}`, position })),
  }) }))
}
