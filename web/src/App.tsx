import { AdminPanel } from "@/components/admin-panel"
import { WorkspaceApp } from "@/components/workspace-app"
import { Route, Routes } from "react-router"

export default function App() {
  return <Routes>
    <Route path="/admin" element={<AdminPanel />} />
    <Route path="/orgs/:organizationId/workspaces/:workspaceId" element={<WorkspaceApp />} />
    <Route path="/orgs/:organizationId" element={<WorkspaceApp />} />
    <Route path="*" element={<WorkspaceApp />} />
  </Routes>
}
