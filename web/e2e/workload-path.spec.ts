import { test, expect } from "@playwright/test"
import { agentPath } from "../src/lib/workload-path"

const agent = { agent_id: "ag-reviewer", name: "reviewer", server_id: "srv-test", kind: "command", status: "running" }
const incompleteTree = { folders: [], placements: [{ agent_id: agent.agent_id, folder_id: "folder-missing", position: 0 }] }

test("incomplete tree never invents a root address for a nested Agent", () => {
  expect(agentPath({ ...agent, path: "/research/reviewer" }, incompleteTree)).toBe("/research/reviewer")
  expect(agentPath(agent, incompleteTree)).toBe("ag-reviewer")
})
