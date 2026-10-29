import { workspacePath } from "@/lib/control-plane"

// Settings are full pages with real URLs. One shell (WorkspaceApp) owns every
// Console route so the workspace stays mounted while a settings page is open.
export const PERSONAL_SECTIONS = ["profile", "sessions", "preferences", "organizations"] as const
export const ORGANIZATION_SECTIONS = ["general", "members", "invitations", "groups", "workspaces", "audit", "danger"] as const
export const WORKSPACE_SECTIONS = ["general", "access", "machines", "launch-profiles", "policy", "audit", "danger"] as const

export type PersonalSection = (typeof PERSONAL_SECTIONS)[number]
export type OrganizationSection = (typeof ORGANIZATION_SECTIONS)[number]
export type WorkspaceSection = (typeof WORKSPACE_SECTIONS)[number]
export type SettingsScope = "personal" | "organization" | "workspace"

export type SettingsRoute =
  | { scope: "personal"; section: string | null }
  | { scope: "organization"; organizationId: string; section: string | null }
  | { scope: "workspace"; organizationId: string; workspaceId: string; section: string | null }

const segment = "([^/]+)"
const tail = "(?:/([^/]+))?/?$"
const personal = new RegExp(`^/settings${tail}`)
const organization = new RegExp(`^/orgs/${segment}/settings${tail}`)
const workspace = new RegExp(`^/orgs/${segment}/workspaces/${segment}/settings${tail}`)

function decode(value: string) {
  try { return decodeURIComponent(value) } catch { return value }
}

export function parseSettingsRoute(pathname: string): SettingsRoute | null {
  let match = workspace.exec(pathname)
  if (match) return { scope: "workspace", organizationId: decode(match[1]), workspaceId: decode(match[2]), section: match[3] ? decode(match[3]) : null }
  match = organization.exec(pathname)
  if (match) return { scope: "organization", organizationId: decode(match[1]), section: match[2] ? decode(match[2]) : null }
  match = personal.exec(pathname)
  if (match) return { scope: "personal", section: match[1] ? decode(match[1]) : null }
  return null
}

export const isSettingsPath = (pathname: string) => parseSettingsRoute(pathname) !== null

export function personalSettingsPath(section?: PersonalSection | null) {
  return section ? `/settings/${section}` : "/settings"
}

export function organizationSettingsPath(organizationId: string, section?: OrganizationSection | null) {
  const base = `/orgs/${encodeURIComponent(organizationId)}/settings`
  return section ? `${base}/${section}` : base
}

export function workspaceSettingsPath(organizationId: string, workspaceId: string, section?: WorkspaceSection | null) {
  const base = `${workspacePath(organizationId, workspaceId)}/settings`
  return section ? `${base}/${section}` : base
}

export function validSection(route: SettingsRoute): boolean {
  if (route.section === null) return true
  const sections: readonly string[] = route.scope === "personal" ? PERSONAL_SECTIONS : route.scope === "organization" ? ORGANIZATION_SECTIONS : WORKSPACE_SECTIONS
  return sections.includes(route.section)
}

export function defaultSection(scope: SettingsScope) {
  return scope === "personal" ? "profile" : "general"
}

// The list page of a scope: phones show the grouped navigation there.
export function settingsListPath(route: SettingsRoute) {
  if (route.scope === "personal") return personalSettingsPath()
  if (route.scope === "organization") return organizationSettingsPath(route.organizationId)
  return workspaceSettingsPath(route.organizationId, route.workspaceId)
}
