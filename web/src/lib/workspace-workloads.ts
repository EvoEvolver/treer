import type { Agent, AppDeployment } from "./api"

// Use the same canonical App identities in Tree, map and Overview.
export function workspaceWorkloads(agents: Agent[], apps: AppDeployment[]): Agent[] {
  const workloads = [...agents]
  for (const app of apps) {
    const id = app.runtime_agent_id ?? app.app_id
    const existing = workloads.findIndex((agent) => agent.agent_id === id || agent.agent_id === app.app_id)
    if (existing >= 0) {
      workloads[existing] = { ...workloads[existing], kind: "app", name: workloads[existing].name === `app:${app.name}` ? app.name : workloads[existing].name }
    } else workloads.push({ agent_id: id, name: app.name, server_id: app.server_id, kind: "app", status: app.status })
  }
  return workloads
}
