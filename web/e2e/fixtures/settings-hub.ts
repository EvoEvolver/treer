import type { Page, Route } from '@playwright/test'

// A stateful Proxy double for the settings hub, including the Core admin
// contract (invitations, leave/transfer/delete, sessions, password,
// effective access, workspace delete with machine revocation).
export type HubOptions = {
  orgRole?: 'owner' | 'admin' | 'member'
  effective?: boolean
  invitations?: boolean
  sessions?: boolean
  orgDeleteConflict?: boolean
  appCapabilities?: string[] | null
  // /api/health "capabilities"; omitted means an older Proxy (health 404).
  healthCapabilities?: string[]
  // Adds a pending invitation an Agent created.
  agentInvitation?: boolean
}

export type Call = { method: string; path: string; body: unknown }

const now = '2026-10-28T10:00:00Z'
const build = { version: '0.2.1', git_commit: 'abcdef1234567890' }

export async function mockHub(page: Page, options: HubOptions = {}) {
  const orgRole = options.orgRole ?? 'owner'
  const calls: Call[] = []
  const user = { user_id: 'u1', email: 'dana@example.test', preferred_name: 'Dana Owner' }
  let organizations = [
    { organization_id: 'org-acme', name: 'Acme Research', role: orgRole },
    { organization_id: 'org-side', name: 'Side Project', role: 'member' as const },
  ]
  let members = [
    { user_id: 'u1', email: user.email, preferred_name: user.preferred_name, role: orgRole },
    { user_id: 'u2', email: 'kenji@example.test', preferred_name: 'Kenji Sato', role: orgRole === 'owner' ? 'admin' : 'owner' },
    { user_id: 'u3', email: 'priya@example.test', preferred_name: 'Priya Raman', role: 'member' },
    { user_id: 'u4', email: 'lucas@example.test', preferred_name: 'Lucas Martin', role: 'member' },
  ]
  let groups = [
    { group_id: 'grp-eng', organization_id: 'org-acme', name: 'Engineering', member_ids: ['u2', 'u3'] },
    { group_id: 'grp-sec', organization_id: 'org-acme', name: 'Security', member_ids: ['u3', 'u1'] },
  ]
  const workspaces: Record<string, { workspace_id: string; name: string; organization_id: string }[]> = {
    'org-acme': [{ workspace_id: 'ws-main', name: 'Main', organization_id: 'org-acme' }, { workspace_id: 'ws-sec', name: 'Security review', organization_id: 'org-acme' }],
    'org-side': [{ workspace_id: 'ws-side', name: 'Sandbox', organization_id: 'org-side' }],
  }
  const manager = orgRole !== 'member'
  const access: Record<string, any> = {
    'ws-main': { workspace_id: 'ws-main', access_mode: 'organization', current_role: manager ? 'owner' : 'member', members: [], groups: [] },
    'ws-sec': { workspace_id: 'ws-sec', access_mode: 'restricted', current_role: 'owner', members: [{ user_id: 'u4', preferred_name: 'Lucas Martin', email: 'lucas@example.test', role: 'member' }], groups: [{ group_id: 'grp-sec', name: 'Security', role: 'owner', member_count: 2 }] },
    'ws-side': { workspace_id: 'ws-side', access_mode: 'organization', current_role: 'member', members: [], groups: [] },
  }
  let invitations = [
    { id: 'inv-1', email: 'chloe@example.test', role: 'member', status: 'pending', created_at: now, expires_at: '2026-11-04T10:00:00Z', created_by: { id: 'u1', name: 'Dana Owner' } },
    { id: 'inv-0', email: 'old@example.test', role: 'member', status: 'accepted', created_at: now, expires_at: now, created_by: { id: 'u1', name: 'Dana Owner' } },
    ...(options.agentInvitation ? [{ id: 'inv-agent', email: 'bot-invite@example.test', role: 'member', status: 'pending', created_at: now, expires_at: '2026-11-04T10:00:00Z', created_by: { id: 'ag-1', name: 'release-bot', kind: 'agent' } }] : []),
  ]
  const machines = [
    { server_id: 'srv-a', name: 'dana-macbook', hostname: 'dana.lan', root: '/Users/dana/treer', controller_build: build, host_build: build, status: 'online' },
    { server_id: 'srv-b', name: 'gpu-node-01', hostname: 'gpu01', root: '/srv/treer', controller_build: build, host_build: build, status: 'offline' },
  ]
  const agents = [
    { agent_id: 'ag-1', server_id: 'srv-a', name: 'leader', path: '/leader', kind: 'command', status: 'idle' },
    { agent_id: 'ag-2', server_id: 'srv-b', name: 'trainer', path: '/trainer', kind: 'command', status: 'idle' },
    { agent_id: 'app-rt', server_id: 'srv-a', name: 'docs', path: '/docs', kind: 'app', status: 'running' },
  ]
  const app = { app_id: 'app-docs', workspace_id: 'ws-main', runtime_agent_id: 'app-rt', name: 'docs', server_id: 'srv-a', command: 'python3', args: [], cwd: '.', port: 9000, hostname: 'docs.test', service_id: 'svc-docs', desired_state: 'running', restart_count: 0, status: 'running', created_at: now, created_by: 'u1', updated_at: now, updated_by: 'u1', ...(options.appCapabilities === null || options.appCapabilities === undefined ? {} : { ui_capabilities: options.appCapabilities }) }
  let sessions = [
    { id: 'ses-1', device: 'Chrome on macOS', ip: '203.0.113.7', created_at: now, last_seen_at: now, current: true },
    { id: 'ses-2', device: 'Safari on iPhone', ip: '198.51.100.2', created_at: now, last_seen_at: now, current: false },
  ]
  const effective = (id: string) => {
    const grants = access[id]
    const result = members.flatMap((member) => {
      const via: { kind: string; detail: string }[] = []
      let owner = false
      if (member.role !== 'member') { via.push({ kind: 'org_role', detail: member.role }); owner = true }
      const direct = grants.members.find((grant: any) => grant.user_id === member.user_id)
      if (direct) { via.push({ kind: 'direct', detail: direct.role }); owner ||= direct.role === 'owner' }
      for (const grant of grants.groups) if (groups.find((group) => group.group_id === grant.group_id)?.member_ids.includes(member.user_id)) { via.push({ kind: 'group', detail: `${grant.name} · ${grant.role}` }); owner ||= grant.role === 'owner' }
      if (grants.access_mode === 'organization' && !via.some((item) => item.kind === 'org_role')) via.unshift({ kind: 'org_role', detail: 'organization' })
      return via.length ? [{ user: { id: member.user_id, name: member.preferred_name, email: member.email }, role: owner ? 'owner' : 'member', via }] : []
    })
    return { members: result, mode: grants.access_mode }
  }
  const ok = (route: Route, body: unknown) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) })
  const missing = (route: Route) => route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ error: { code: 'not_found', message: 'not found' } }) })
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/events$/, () => {})
  await page.route('**/api/**', async (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api/, '')
    const method = route.request().method()
    let body: unknown = null
    try { body = route.request().postDataJSON() } catch { body = route.request().postData() }
    if (method !== 'GET') calls.push({ method, path, body })
    if (path === '/health') return options.healthCapabilities ? ok(route, { status: 'ok', service: 'treer-proxy', capabilities: options.healthCapabilities }) : missing(route)
    if (path === '/invitations/pending') return route.fulfill({ status: 204 })
    if (path === '/auth/me') return ok(route, user)
    if (path === '/auth/config') return ok(route, { github: false, google: false, invitation_required: false })
    if (path === '/auth/profile' && method === 'PATCH') { Object.assign(user, body); return ok(route, user) }
    if (path === '/auth/logout') return ok(route, {})
    if (path === '/auth/sessions') return options.sessions === false ? missing(route) : ok(route, { sessions })
    if (path.startsWith('/auth/sessions/') && method === 'DELETE') { sessions = sessions.filter((item) => item.id !== path.split('/').pop()); return route.fulfill({ status: 204 }) }
    if (path === '/auth/password') return options.sessions === false ? missing(route) : route.fulfill({ status: 204 })
    if (path === '/organizations' && method === 'GET') return ok(route, { organizations })
    let match = path.match(/^\/organizations\/([^/]+)(\/.*)?$/)
    if (match) {
      const [, org, rest = ''] = match
      if (rest === '' && method === 'PATCH') { organizations = organizations.map((item) => item.organization_id === org ? { ...item, name: (body as { name: string }).name } : item); return ok(route, {}) }
      if (rest === '' && method === 'DELETE') {
        if (options.orgDeleteConflict) return route.fulfill({ status: 409, contentType: 'application/json', body: JSON.stringify({ error: { code: 'organization_has_workspaces', message: 'organization still has workspaces' }, workspace_count: 2 }) })
        organizations = organizations.filter((item) => item.organization_id !== org); return route.fulfill({ status: 204 })
      }
      if (rest === '/leave') { organizations = organizations.filter((item) => item.organization_id !== org); return route.fulfill({ status: 204 }) }
      if (rest === '/transfer') { const target = (body as { user_id: string }).user_id; members = members.map((member) => member.user_id === target ? { ...member, role: 'owner' } : member); return ok(route, {}) }
      if (rest === '/members') return ok(route, { members: org === 'org-acme' ? members : [members[0]] })
      if (rest.startsWith('/members/') && method === 'PATCH') { const id = rest.split('/')[2]; members = members.map((member) => member.user_id === id ? { ...member, role: (body as { role: string }).role } : member); return ok(route, {}) }
      if (rest.startsWith('/members/') && method === 'DELETE') { const id = rest.split('/')[2]; members = members.filter((member) => member.user_id !== id); groups = groups.map((group) => ({ ...group, member_ids: group.member_ids.filter((item) => item !== id) })); return ok(route, {}) }
      if (rest === '/groups' && method === 'GET') return ok(route, { groups: org === 'org-acme' ? groups : [] })
      if (rest === '/groups' && method === 'POST') { const group = { group_id: `grp-${groups.length + 1}`, organization_id: org, name: (body as { name: string }).name, member_ids: [] }; groups = [...groups, group]; return ok(route, { group }) }
      const groupMember = rest.match(/^\/groups\/([^/]+)\/members\/([^/]+)$/)
      if (groupMember) { const [, groupId, userId] = groupMember; groups = groups.map((group) => group.group_id !== groupId ? group : { ...group, member_ids: method === 'PUT' ? [...new Set([...group.member_ids, userId])] : group.member_ids.filter((item) => item !== userId) }); return ok(route, { groups }) }
      if (rest.startsWith('/groups/') && method === 'DELETE') { groups = groups.filter((group) => group.group_id !== rest.split('/')[2]); return ok(route, {}) }
      if (rest === '/audit-events') return ok(route, { events: [{ sequence: 2, event_id: 'ev-2', organization_id: org, occurred_at: now, actor_kind: 'agent', actor_id: 'ag-1', actor_name: 'release-bot', source: 'api', action: 'invitation.created', outcome: 'succeeded', resource_kind: 'invitation', resource_id: 'inv-agent', payload: {} }, { sequence: 1, event_id: 'ev-1', organization_id: org, occurred_at: now, actor_kind: 'user', actor_id: 'u1', actor_name: 'Dana Owner', source: 'console', action: 'member.role_updated', outcome: 'succeeded', resource_kind: 'member', resource_id: 'u2', resource_name: 'Kenji Sato', payload: {} }] })
      if (rest === '/invitations' && options.invitations === false) return method === 'POST' ? ok(route, { url: 'https://treer.test/?invite=legacy' }) : route.fulfill({ status: 405, body: '' })
      if (rest === '/invitations' && method === 'GET') return ok(route, { invitations })
      if (rest === '/invitations' && method === 'POST') {
        const request = body as { email?: string; role: string }
        const invitation = { id: `inv-${invitations.length + 1}`, email: request.email ?? null, role: request.role, status: 'pending', created_at: now, expires_at: '2026-11-04T10:00:00Z', created_by: { id: 'u1', name: 'Dana Owner' } }
        invitations = [invitation, ...invitations]
        return ok(route, { ...invitation, url: `https://treer.test/?invite=${invitation.id}`, email_queued: Boolean(request.email) })
      }
      if (rest.startsWith('/invitations/') && method === 'DELETE') { const id = rest.split('/')[2]; invitations = invitations.map((item) => item.id === id ? { ...item, status: 'revoked' } : item); return route.fulfill({ status: 204 }) }
    }
    if (path === '/workspaces' && method === 'GET') return ok(route, { workspaces: workspaces[url.searchParams.get('organization_id') ?? ''] ?? [] })
    match = path.match(/^\/workspaces\/([^/]+)(\/.*)?$/)
    if (match) {
      const [, ws, rest = ''] = match
      if (rest === '' && method === 'PATCH') { for (const list of Object.values(workspaces)) for (const item of list) if (item.workspace_id === ws) item.name = (body as { name: string }).name; return ok(route, { workspace: Object.values(workspaces).flat().find((item) => item.workspace_id === ws) }) }
      if (rest === '' && method === 'DELETE') { for (const key of Object.keys(workspaces)) workspaces[key] = workspaces[key].filter((item) => item.workspace_id !== ws); return route.fulfill({ status: 204 }) }
      if (rest === '/snapshot') return ok(route, { revision: 1, workspace: Object.values(workspaces).flat().find((item) => item.workspace_id === ws), servers: ws === 'ws-main' ? machines : [], agents: ws === 'ws-main' ? agents : [] })
      if (rest === '/apps') return ok(route, { apps: ws === 'ws-main' ? [app] : [] })
      if (rest === '/workload-tree') return ok(route, { folders: [], placements: [] })
      if (rest === '/launch-profiles') return ok(route, { profiles: [] })
      if (rest === '/access' && method === 'GET') return access[ws] ? ok(route, { access: access[ws] }) : missing(route)
      if (rest === '/access' && method === 'PATCH') { access[ws] = { ...access[ws], access_mode: (body as { access_mode: string }).access_mode }; return ok(route, { access: access[ws] }) }
      if (rest === '/access/effective') return options.effective === false || !access[ws] ? missing(route) : ok(route, effective(ws))
      const grant = rest.match(/^\/access\/(users|groups)\/([^/]+)$/)
      if (grant) {
        const [, kind, id] = grant
        const key = kind === 'users' ? 'members' : 'groups'
        const field = kind === 'users' ? 'user_id' : 'group_id'
        const rest2 = access[ws][key].filter((item: any) => item[field] !== id)
        if (method === 'PUT') {
          const role = (body as { role: string }).role
          const member = members.find((item) => item.user_id === id)
          const group = groups.find((item) => item.group_id === id)
          rest2.push(kind === 'users' ? { user_id: id, preferred_name: member?.preferred_name, email: member?.email, role } : { group_id: id, name: group?.name, role, member_count: group?.member_ids.length ?? 0 })
        }
        access[ws] = { ...access[ws], [key]: rest2 }
        return ok(route, { access: access[ws] })
      }
      if (rest === '/policy-provider' && method === 'GET') return ok(route, { fallback: { kind: 'treer_default', name: 'Treer Default', mode: 'monitor', effect: 'allow' }, cache: {} })
      if (/^\/servers\/[^/]+$/.test(rest) && method === 'DELETE') return ok(route, {})
      if (rest === '/services') return ok(route, { services: ws === 'ws-main' ? [{ service_id: 'svc-1', name: 'api', server_id: 'srv-a', target_host: '127.0.0.1', target_port: 3000, protocol: 'http', updated_at: now, updated_by: 'u1' }] : [] })
      if (/^\/services\/[^/]+\/probe$/.test(rest)) return ok(route, { healthy: true })
      if (rest === '/virtual-hosts') return ok(route, { hosts: [] })
      if (rest === '/traffic') return ok(route, { traffic: [] })
      if (rest.startsWith('/audit-events')) return ok(route, { events: ws === 'ws-main' ? [{ sequence: 7, event_id: 'wev-7', workspace_id: ws, occurred_at: now, actor_kind: 'agent', actor_id: 'ag-1', actor_name: 'release-bot', action: 'agent.renamed', outcome: 'succeeded', resource_kind: 'agent', resource_id: 'ag-2' }] : [], next_cursor: null })
      if (rest.startsWith('/apps/app-docs/ui/api/tree')) return ok(route, { schema_version: 1, path: [], nodes: [] })
    }
    return missing(route)
  })
  return { calls, get invitations() { return invitations }, get members() { return members }, get groups() { return groups }, access }
}

export const noHorizontalScroll = (page: Page) => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)
