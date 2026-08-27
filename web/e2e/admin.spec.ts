import { test, expect, type Page, type Route } from "@playwright/test"

function ok(route: Route, body: unknown, status = 200) {
  return route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })
}

const user = {
  user_id: "usr_ada",
  email: "ada@example.com",
  preferred_name: "Ada",
  email_verified: false,
  created_at: "2026-08-25T00:00:00Z",
}

async function mockAdmin(page: Page) {
  await page.route("**/api/admin/**", async (route: Route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api\/admin/, "")
    const method = route.request().method()
    if (path === "/me") return ok(route, { admin: true })
    if (path === "/dashboard") {
      return ok(route, { user_count: 1, organization_count: 1, machine_count: 1, agent_count: 1 })
    }
    if (path === "/users" && method === "GET") return ok(route, { users: [user] })
    if (path === "/users/usr_ada" && method === "GET") {
      return ok(route, {
        user: {
          ...user,
          password_login: true,
          oauth_providers: [],
          organizations: [{ organization_id: "org_1", name: "Ada Personal", role: "owner" }],
          workspaces: [{ workspace_id: "ws_1", name: "lab", organization_id: "org_1" }],
        },
      })
    }
    if (path === "/users/usr_ada/password-reset" && method === "POST") {
      return ok(route, { url: "https://app.example/?reset=pwd_test.secret", emailed: false })
    }
    if (path === "/machines" && method === "GET") {
      return ok(route, {
        machines: [{
          server_id: "srv_1",
          name: "builder",
          hostname: "builder.lan",
          workspace_id: "ws_1",
          workspace_name: "lab",
          created_at: "2026-08-25T00:00:00Z",
          enrolled_by: "usr_ada",
          status: "online",
        }],
      })
    }
    if (path === "/machines/srv_1" && method === "GET") {
      return ok(route, {
        machine: {
          server_id: "srv_1",
          name: "builder",
          hostname: "builder.lan",
          workspace_id: "ws_1",
          workspace_name: "lab",
          created_at: "2026-08-25T00:00:00Z",
          enrolled_by: "usr_ada",
          status: "online",
          agents: [{
            agent_id: "ag_1",
            server_id: "srv_1",
            name: "reviewer",
            kind: "codex",
            status: "idle",
            interface: { protocol: "treer.agent-interface/v1", instance_id: "codex_1", port: 1, capabilities: ["prompt.submit"], registered_at: "2026-08-25T00:00:00Z" },
          }],
        },
      })
    }
    return ok(route, { error: { message: "not found" } }, 404)
  })
}

test("admin cards expand user and machine inventories", async ({ page }) => {
  await mockAdmin(page)
  await page.goto("/admin")
  await expect(page.getByRole("heading", { name: "Platform overview" })).toBeVisible()
  await page.getByRole("button", { name: /Users/ }).click()
  await expect(page.getByText("Ada", { exact: true })).toBeVisible()
  await expect(page.getByText("ada@example.com")).toBeVisible()
  await page.getByRole("button", { name: "Reset password" }).click()
  await expect(page.getByRole("heading", { name: "Password reset" })).toBeVisible()
  await expect(page.getByText("pwd_test.secret")).toBeVisible()
  await page.getByRole("button", { name: "Close", exact: true }).first().click()
  await page.getByRole("button", { name: /Machines/ }).click()
  await expect(page.getByText("builder.lan")).toBeVisible()
  await page.getByRole("button", { name: /builder/ }).click()
  await expect(page.getByText("reviewer")).toBeVisible()
  await expect(page.getByText("codex")).toBeVisible()
})
