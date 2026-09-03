import { driver, type DriveStep } from "driver.js"
import "driver.js/dist/driver.css"

export const FIRST_RUN_TOUR_VERSION = "v1"

export type SidebarTab = "apps" | "agents"

export type FirstRunTourHost = {
  setSidebarTab: (tab: SidebarTab) => void
  openCreateWorkspace: () => void
  closeCreateWorkspace: () => void
  prepareWorkspaceForMachineSteps: () => void
  openWorkspaceSettings: () => void
  closeWorkspaceSettings: () => void
  openInstall: () => void | Promise<void>
  closeInstall: () => void
  openCreateAgent: (reset: boolean) => void
  closeCreateAgent: () => void
  setAgentLaunch: (kind: "terminal" | "recipe" | "ui-profile") => void
}

const STORAGE_PREFIX = `treer.first-run-tour.${FIRST_RUN_TOUR_VERSION}`
const ADMIN_STORAGE_KEY = `treer.admin-tour.${FIRST_RUN_TOUR_VERSION}`

let activeTour: ReturnType<typeof driver> | null = null

export type AdminTourHost = {
  openInvite: () => void
  closeInvite: () => void
}

export function firstRunTourMode(): "preview" | "force" | null {
  const value = new URLSearchParams(window.location.search).get("tour")
  if (value === "preview") return "preview"
  if (value === "1" || value === "start" || value === "replay") return "force"
  return null
}

export function firstRunTourStorageKey(userId: string) {
  return `${STORAGE_PREFIX}.${userId}`
}

export function hasCompletedFirstRunTour(userId: string) {
  try {
    return window.localStorage.getItem(firstRunTourStorageKey(userId)) === "done"
  } catch {
    return false
  }
}

export function markFirstRunTourComplete(userId: string) {
  try {
    window.localStorage.setItem(firstRunTourStorageKey(userId), "done")
  } catch {
    /* ignore quota / private mode */
  }
}

export function clearFirstRunTour(userId: string) {
  try {
    window.localStorage.removeItem(firstRunTourStorageKey(userId))
  } catch {
    /* ignore */
  }
}

function waitForSelector(selector: string, timeoutMs = 2500) {
  return new Promise<Element>((resolve, reject) => {
    const existing = document.querySelector(selector)
    if (existing) {
      resolve(existing)
      return
    }
    const timeout = window.setTimeout(() => {
      observer.disconnect()
      reject(new Error(`tour target not found: ${selector}`))
    }, timeoutMs)
    const observer = new MutationObserver(() => {
      const node = document.querySelector(selector)
      if (!node) return
      window.clearTimeout(timeout)
      observer.disconnect()
      resolve(node)
    })
    observer.observe(document.body, { childList: true, subtree: true })
  })
}

async function goTo(tour: ReturnType<typeof driver>, selector: string, prepare: () => void | Promise<void>) {
  await prepare()
  await waitForSelector(selector)
  await new Promise((resolve) => window.requestAnimationFrame(() => resolve(undefined)))
  tour.moveNext()
}

function shouldAutoStartTour(completed: boolean) {
  const mode = firstRunTourMode()
  if (mode === "preview" || mode === "force") return true
  if (typeof navigator !== "undefined" && navigator.webdriver) return false
  return !completed
}

export function shouldAutoStartFirstRunTour(userId: string) {
  return shouldAutoStartTour(hasCompletedFirstRunTour(userId))
}

export function hasCompletedAdminTour() {
  try {
    return window.localStorage.getItem(ADMIN_STORAGE_KEY) === "done"
  } catch {
    return false
  }
}

export function markAdminTourComplete() {
  try {
    window.localStorage.setItem(ADMIN_STORAGE_KEY, "done")
  } catch {
    /* ignore quota / private mode */
  }
}

export function clearAdminTour() {
  try {
    window.localStorage.removeItem(ADMIN_STORAGE_KEY)
  } catch {
    /* ignore */
  }
}

export function shouldAutoStartAdminTour() {
  return shouldAutoStartTour(hasCompletedAdminTour())
}

export function stopFirstRunTour() {
  activeTour?.destroy()
  activeTour = null
}

export function startFirstRunTour(host: FirstRunTourHost, options: { userId: string; persist: boolean }) {
  stopFirstRunTour()

  const tour = driver({
    showProgress: true,
    progressText: "{{current}} of {{total}}",
    nextBtnText: "Next",
    prevBtnText: "Back",
    doneBtnText: "Done",
    allowClose: true,
    overlayOpacity: 0.55,
    stagePadding: 8,
    stageRadius: 6,
    popoverOffset: 12,
    popoverClass: "treer-tour-popover",
    onDestroyStarted: () => {
      if (options.persist) markFirstRunTourComplete(options.userId)
      tour.destroy()
    },
    onDestroyed: () => {
      host.closeCreateWorkspace()
      host.closeInstall()
      host.closeCreateAgent()
      host.closeWorkspaceSettings()
      host.setSidebarTab("agents")
      if (activeTour === tour) activeTour = null
    },
  })

  const steps: DriveStep[] = [
    {
      popover: {
        title: "Welcome to Treer",
        description:
          "<p>Treer is a control plane for coding agents on machines you already own. This tour covers the first three things you need: a <strong>workspace</strong>, a <strong>machine</strong>, and an <strong>agent</strong>.</p>",
      },
    },
    {
      element: "[data-tour='workspace-select']",
      popover: {
        title: "What is a workspace?",
        description:
          "<p>A workspace is a shared room for one project or lab. Machines you enroll, agents you start, and launch profiles all live here. Organization members who can see the workspace can use those machines and agents. It is not the git checkout on disk — each machine still has its own working directory.</p>",
        side: "right",
        align: "start",
      },
    },
    {
      element: "[data-tour='create-workspace']",
      popover: {
        title: "Create a workspace",
        description:
          "<p>Treer does not create a workspace for you. Use <strong>+</strong> to add one — for example <code>personal</code> or <code>lab</code>. You can rename it later; the workspace ID stays the same, so enrolled machines stay connected.</p>",
        side: "bottom",
        align: "end",
        onNextClick: () => {
          void goTo(tour, "[data-tour='create-workspace-dialog']", () => {
            host.openCreateWorkspace()
          })
        },
      },
    },
    {
      element: "[data-tour='create-workspace-dialog']",
      popover: {
        title: "Name the workspace",
        description:
          "<p>The name is only a label shown to members. After you create it, this control plane can enroll machines into that workspace and start agents on them. Close this dialog when you are ready — you do not have to create one during the tour.</p>",
        side: "left",
        align: "start",
        onPrevClick: () => {
          host.closeCreateWorkspace()
          tour.movePrevious()
        },
        onNextClick: () => {
          void goTo(tour, "[data-tour='workspace-machines']", () => {
            host.closeCreateWorkspace()
            host.prepareWorkspaceForMachineSteps()
            host.openWorkspaceSettings()
          })
        },
      },
    },
    {
      element: "[data-tour='workspace-machines']",
      popover: {
        title: "Machines hold the processes",
        description:
          "<p>Agents do not run in the browser. They run on enrolled machines: a laptop, a workstation, or a server. The machine connects <strong>out</strong> to this Proxy, so you do not publish SSH or local agent ports.</p>",
        side: "right",
        align: "start",
        onPrevClick: () => {
          host.closeWorkspaceSettings()
          host.setSidebarTab("agents")
          host.openCreateWorkspace()
          void waitForSelector("[data-tour='create-workspace-dialog']").then(() => tour.movePrevious())
        },
      },
    },
    {
      element: "[data-tour='add-machine']",
      popover: {
        title: "Add your first machine",
        description:
          "<p>Open <strong>Machines</strong> and click <strong>Add</strong>. That dialog is how you enroll the first device into this workspace. You need an online machine before you can start an agent.</p>",
        side: "bottom",
        align: "end",
        onNextClick: () => {
          void goTo(tour, "[data-tour='add-machine-dialog']", () => host.openInstall())
        },
      },
    },
    {
      element: "[data-tour='add-machine-dialog']",
      popover: {
        title: "Install, then enroll",
        description:
          "<p><strong>Step 1</strong> installs Treer on that computer. It has no secret and can be reused.</p><p><strong>Step 2</strong> is a 10-minute, single-use enrollment key. Run it on the same machine to bind that Host to this workspace. Copy both commands to the device, then come back here — the machine appears when it is online.</p>",
        side: "left",
        align: "start",
        onPrevClick: () => {
          host.closeInstall()
          tour.movePrevious()
        },
        onNextClick: () => {
          void goTo(tour, "[data-tour='create-agent']", () => {
            host.closeInstall()
            host.setSidebarTab("agents")
          })
        },
      },
    },
    {
      element: "[data-tour='create-agent']",
      popover: {
        title: "Start an agent",
        description:
          "<p>Once a machine is online, open <strong>Agents</strong> and click <strong>New</strong>. An agent is a process Treer keeps alive on that machine: a shell, a coding agent, or a short-lived installer. This button stays disabled until at least one machine is online.</p>",
        side: "bottom",
        align: "end",
        onPrevClick: () => {
          host.openWorkspaceSettings()
          void host.openInstall()
          void waitForSelector("[data-tour='add-machine-dialog']").then(() => tour.movePrevious())
        },
        onNextClick: () => {
          void goTo(tour, "[data-tour='agent-launch']", () => host.openCreateAgent(true))
        },
      },
    },
    {
      element: "[data-tour='agent-launch']",
      popover: {
        title: "Command-line agents",
        description:
          "<p><strong>Terminal</strong> starts your interactive shell. <strong>Custom command</strong> runs an executable you type, such as <code>codex</code>.</p><p>These are PTY sessions: you type in the terminal, and Treer streams that output. No extra UI is required.</p>",
        side: "left",
        align: "start",
        onPrevClick: () => {
          host.closeCreateAgent()
          tour.movePrevious()
        },
        onNextClick: () => {
          host.setAgentLaunch("ui-profile")
          tour.moveNext()
        },
      },
    },
    {
      element: "[data-tour='agent-launch']",
      popover: {
        title: "UI agents (AIS / ACP-style)",
        description:
          "<p>Saved profiles such as <strong>Codex</strong>, <strong>Claude</strong>, <strong>Pi</strong>, and <strong>OpenCode</strong> start a coding agent that can register an <strong>Agent Interface Server</strong> (AIS), similar to ACP.</p><p>If that agent publishes a <code>ui_path</code>, Treer embeds the page instead of the terminal. Prompts and transcripts go through the interface, not by typing into the shell. The list shows <code>· AIS</code> when that happens.</p>",
        side: "left",
        align: "start",
        onPrevClick: () => {
          host.setAgentLaunch("terminal")
          tour.movePrevious()
        },
        onNextClick: () => {
          void goTo(tour, "[data-tour='agent-recipe']", () => host.setAgentLaunch("recipe"))
        },
      },
    },
    {
      element: "[data-tour='agent-recipe']",
      popover: {
        title: "What an installer agent is",
        description:
          "<p><strong>Install recipe</strong> starts a short-lived coding agent and gives it Treer’s install skill plus a git URL. It clones the recipe, creates a <em>different</em> command agent, and saves a launch profile.</p><p>The installer is not the app. After it finishes, use <strong>Launch</strong> on that profile to start a real agent — with a UI if the recipe registered one. Do not run Install recipe again to open another conversation.</p>",
        side: "left",
        align: "start",
        onPrevClick: () => {
          host.setAgentLaunch("ui-profile")
          tour.movePrevious()
        },
        onNextClick: () => {
          host.closeCreateAgent()
          tour.moveNext()
        },
      },
    },
    {
      popover: {
        title: "You’re ready",
        description:
          "<p>Create a workspace, enroll a machine with the two commands, then start a terminal or a saved profile. Replay this tour anytime from the user menu.</p>",
        onPrevClick: () => {
          void (async () => {
            host.openCreateAgent(false)
            host.setAgentLaunch("recipe")
            await waitForSelector("[data-tour='agent-recipe']")
            tour.movePrevious()
          })()
        },
      },
    },
  ]

  tour.setSteps(steps)
  activeTour = tour
  tour.drive()
}

export function startAdminTour(host: AdminTourHost, options: { persist: boolean }) {
  stopFirstRunTour()

  const tour = driver({
    showProgress: true,
    progressText: "{{current}} of {{total}}",
    nextBtnText: "Next",
    prevBtnText: "Back",
    doneBtnText: "Done",
    allowClose: true,
    overlayOpacity: 0.55,
    stagePadding: 8,
    stageRadius: 6,
    popoverOffset: 12,
    popoverClass: "treer-tour-popover",
    onDestroyStarted: () => {
      if (options.persist) markAdminTourComplete()
      tour.destroy()
    },
    onDestroyed: () => {
      host.closeInvite()
      if (activeTour === tour) activeTour = null
    },
  })

  const steps: DriveStep[] = [
    {
      popover: {
        title: "Platform administration",
        description:
          "<p>This panel is for the <strong>platform administrator</strong>, not a Treer user. It is not a workspace. You invite people here; they register, get a personal organization, and then enroll machines.</p>",
      },
    },
    {
      element: "[data-tour='admin-overview']",
      popover: {
        title: "What you are looking at",
        description:
          "<p>These totals are live across every organization on this Proxy: enrolled machines and running agents. They are a health snapshot, not a place to start work.</p>",
        side: "bottom",
        align: "start",
      },
    },
    {
      element: "[data-tour='admin-invite']",
      popover: {
        title: "Invite the first user",
        description:
          "<p>New accounts need an invitation unless open registration is enabled. <strong>Create invitation</strong> mints a one-time registration link. That user gets an organization named <code>&lt;preferred name&gt; Personal</code> and owns it.</p>",
        side: "top",
        align: "end",
        onNextClick: () => {
          void goTo(tour, "[data-tour='admin-invite-dialog']", () => host.openInvite())
        },
      },
    },
    {
      element: "[data-tour='admin-invite-dialog']",
      popover: {
        title: "Send the link, then stop",
        description:
          "<p>Copy this URL to the person who should join. It can be used once. After they register, they create a workspace and enroll machines themselves. You do not enroll devices from this admin panel.</p>",
        side: "left",
        align: "start",
        onPrevClick: () => {
          host.closeInvite()
          tour.movePrevious()
        },
        onNextClick: () => {
          host.closeInvite()
          tour.moveNext()
        },
      },
    },
    {
      element: "[data-tour='admin-workspace-link']",
      popover: {
        title: "Work happens in the app",
        description:
          "<p>Use <strong>User workspace</strong> to open the normal Treer app. Log in as an invited user there to create a workspace, add a machine, and start an agent. Replay this tour with the button next to Refresh.</p>",
        side: "bottom",
        align: "end",
      },
    },
  ]

  tour.setSteps(steps)
  activeTour = tour
  tour.drive()
}
