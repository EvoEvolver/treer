// Only folder IDs and booleans are stored; paths and Agent data never enter this cache.
export type FolderExpansion = Record<string, boolean>
export const folderExpansionKey = (workspaceId: string) => `treer:folder-expansion:v1:${workspaceId}`

export function readFolderExpansion(workspaceId: string | null): FolderExpansion {
  if (!workspaceId) return {}
  try {
    const value: unknown = JSON.parse(window.localStorage.getItem(folderExpansionKey(workspaceId)) ?? '{}')
    if (!value || typeof value !== 'object' || Array.isArray(value)) return {}
    return Object.fromEntries(Object.entries(value).filter(([, expanded]) => typeof expanded === 'boolean'))
  } catch { return {} }
}

export function writeFolderExpansion(workspaceId: string | null, value: FolderExpansion) {
  if (!workspaceId) return
  try { window.localStorage.setItem(folderExpansionKey(workspaceId), JSON.stringify(value)) } catch { /* In-memory toggles still work. */ }
}
