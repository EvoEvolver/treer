import { driver, type DriveStep } from "driver.js"
import "driver.js/dist/driver.css"
import { t } from "@/i18n"

export const FIRST_RUN_TOUR_VERSION = "v1"

export type FirstRunTourHost = {
  openCreateWorkspace: () => void
  closeCreateWorkspace: () => void
  prepareWorkspaceForMachineSteps: () => void
  openWorkspaceSettings: () => void
  closeWorkspaceSettings: () => void
  openInstall: () => void | Promise<void>
  closeInstall: () => void
  openCreateAgent: (reset: boolean) => void
  closeCreateAgent: () => void
  setAgentLaunch: (kind: "terminal" | "ui-profile") => void
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
    progressText: t("tour.current_of_total"),
    nextBtnText: t("tour.next"),
    prevBtnText: t("org.back"),
    doneBtnText: t("tabs.done"),
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
      if (activeTour === tour) activeTour = null
    },
  })

  const steps: DriveStep[] = [
    {
      popover: {
        title: t("tour.welcome_to_treer"),
        description:
          t("tour.treer_is_a_control_plane"),
      },
    },
    {
      element: "[data-tour='workspace-select']",
      popover: {
        title: t("tour.what_is_a_workspace"),
        description:
          t("tour.a_workspace_is_a_shared"),
        side: "right",
        align: "start",
      },
    },
    {
      element: "[data-tour='create-workspace']",
      popover: {
        title: t("tour.create_a_workspace"),
        description:
          t("tour.treer_does_not_create_a"),
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
        title: t("tour.name_the_workspace"),
        description:
          t("tour.the_name_is_only_a"),
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
        title: t("tour.machines_hold_the_processes"),
        description:
          t("tour.agents_do_not_run_in"),
        side: "right",
        align: "start",
        onPrevClick: () => {
          host.closeWorkspaceSettings()
          host.openCreateWorkspace()
          void waitForSelector("[data-tour='create-workspace-dialog']").then(() => tour.movePrevious())
        },
      },
    },
    {
      element: "[data-tour='add-machine']",
      popover: {
        title: t("tour.add_your_first_machine"),
        description:
          t("tour.open_machines_and_click_add"),
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
        title: t("tour.install_then_enroll"),
        description:
          t("tour.step_1_installs_treer_on"),
        side: "left",
        align: "start",
        onPrevClick: () => {
          host.closeInstall()
          tour.movePrevious()
        },
        onNextClick: () => {
          void goTo(tour, "[data-tour='create-agent']", () => {
            host.closeInstall()
          })
        },
      },
    },
    {
      element: "[data-tour='create-agent']",
      popover: {
        title: t("tour.start_an_agent"),
        description:
          t("tour.once_a_machine_is_online"),
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
        title: t("tour.command_line_agents"),
        description:
          t("tour.terminal_starts_your_interactive_shell"),
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
        title: t("tour.ui_agents_ais_acp_style"),
        description:
          t("tour.saved_profiles_run_your_chosen"),
        side: "left",
        align: "start",
        onPrevClick: () => {
          host.setAgentLaunch("terminal")
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
        title: t("tour.you_re_ready"),
        description:
          t("tour.create_a_workspace_enroll_a"),
        onPrevClick: () => {
          void (async () => {
            host.openCreateAgent(false)
            host.setAgentLaunch("ui-profile")
            await waitForSelector("[data-tour='agent-launch']")
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
    progressText: t("tour.current_of_total"),
    nextBtnText: t("tour.next"),
    prevBtnText: t("org.back"),
    doneBtnText: t("tabs.done"),
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
        title: t("tour.platform_administration"),
        description:
          t("tour.this_panel_is_for_the"),
      },
    },
    {
      element: "[data-tour='admin-overview']",
      popover: {
        title: t("tour.what_you_are_looking_at"),
        description:
          t("tour.these_totals_are_live_across"),
        side: "bottom",
        align: "start",
      },
    },
    {
      element: "[data-tour='admin-invite']",
      popover: {
        title: t("tour.invite_the_first_user"),
        description:
          t("tour.new_accounts_need_an_invitation"),
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
        title: t("tour.send_the_link_then_stop"),
        description:
          t("tour.copy_this_url_to_the"),
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
        title: t("tour.work_happens_in_the_app"),
        description:
          t("tour.use_user_workspace_to_open"),
        side: "bottom",
        align: "end",
      },
    },
  ]

  tour.setSteps(steps)
  activeTour = tour
  tour.drive()
}
