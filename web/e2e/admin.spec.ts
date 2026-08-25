import { test, expect, type Page, type Route } from "@playwright/test"

function ok(route: Route, body: unknown, status = 200) {
  return route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) })
}

const running = {
  channel: "stable",
  services: [
    {
      name: "proxy",
      present: true,
      version: "v0.1.2",
      digest: "sha256:aaaaaaaaaaaaaaaa",
      update_available: false,
    },
    {
      name: "app",
      present: true,
      version: "v0.1.2",
      digest: "sha256:bbbbbbbbbbbbbbbb",
      update_available: false,
    },
    {
      name: "updater",
      present: true,
      version: "v0.1.2",
      digest: "sha256:cccccccccccccccc",
      update_available: false,
    },
  ],
  job: null,
}

async function mockAdmin(page: Page, mode: "configured" | "unconfigured") {
  await page.route("**/api/admin/**", async (route: Route) => {
    const url = new URL(route.request().url())
    const path = url.pathname.replace(/^\/api\/admin/, "")
    const method = route.request().method()
    if (path === "/me") return ok(route, { admin: true })
    if (path === "/dashboard") return ok(route, { machine_count: 2, agent_count: 4 })
    if (path === "/update" && method === "GET") {
      if (mode === "unconfigured") {
        return ok(route, { error: { code: "updater_unconfigured", message: "this deployment does not run a control-plane updater sidecar" } }, 404)
      }
      return ok(route, running)
    }
    if (path === "/update/check" && method === "GET") {
      return ok(route, {
        ...running,
        update_available: true,
        services: running.services.map((service) =>
          service.name === "proxy" ? { ...service, update_available: true, channel_digest: "sha256:dddddddddddddddd" } : service,
        ),
      })
    }
    if (path === "/update" && method === "POST") {
      return ok(route, { ...running, job: { id: "job1", state: "running", error: null } }, 202)
    }
    return ok(route, { error: { message: "not found" } }, 404)
  })
}

test("admin panel shows control-plane updates when the sidecar is configured", async ({ page }) => {
  await mockAdmin(page, "configured")
  await page.goto("/admin")
  await expect(page.getByRole("heading", { name: "Platform overview" })).toBeVisible()
  await expect(page.getByRole("heading", { name: "Control plane" })).toBeVisible()
  await expect(page.getByText("Channel stable")).toBeVisible()
  await page.getByRole("button", { name: "Check for updates" }).click()
  await expect(page.getByText("update available")).toBeVisible()
  await expect(page.getByRole("button", { name: "Apply update" })).toBeEnabled()
})

test("admin panel explains when control-plane updates are not configured", async ({ page }) => {
  await mockAdmin(page, "unconfigured")
  await page.goto("/admin")
  await expect(page.getByRole("heading", { name: "Control plane" })).toBeVisible()
  await expect(page.getByText("Control-plane updates are not configured on this deployment.")).toBeVisible()
  await expect(page.getByRole("button", { name: "Apply update" })).toHaveCount(0)
})
