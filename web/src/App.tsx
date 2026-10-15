import { AdminPanel } from "@/components/admin-panel"
import { WorkspaceApp } from "@/components/workspace-app"
import { useLocale } from "@/i18n"
import { Route, Routes } from "react-router"

export default function App() {
  // One subscription at the root: a language change re-renders the whole tree, so any component can call t() directly.
  useLocale()
  return <Routes>
    <Route path="/admin" element={<AdminPanel />} />
    <Route path="/orgs/:organizationId/workspaces/:workspaceId" element={<WorkspaceApp />} />
    <Route path="/orgs/:organizationId" element={<WorkspaceApp />} />
    <Route path="*" element={<WorkspaceApp />} />
  </Routes>
}
