import { useEffect, useRef, useState, type CSSProperties, type PointerEvent, type KeyboardEvent } from "react"
import { t } from "@/i18n"

const DEFAULT_WIDTH = 272
const MIN_WIDTH = 240
const MAX_WIDTH = 480
const clamp = (value: number, max: number) => Math.round(Math.max(MIN_WIDTH, Math.min(max, value)))

export function useTreeWidth(userId: string | undefined, onFinish: () => void) {
  const [preference, setPreference] = useState(DEFAULT_WIDTH)
  const [collapsed, setCollapsed] = useState(false)
  const [viewport, setViewport] = useState(() => window.innerWidth)
  const [dragging, setDragging] = useState(false)
  const drag = useRef<{ id: number; x: number; width: number } | null>(null)
  const finishRef = useRef(onFinish)
  // The collapse button (user row) and the re-open button (tab bar) are different elements; focus follows the toggle.
  const collapseRef = useRef<HTMLButtonElement>(null)
  const expandRef = useRef<HTMLButtonElement>(null)
  const restoreFocus = useRef(false)
  finishRef.current = onFinish
  const key = userId ? `treer:tree-width:v1:${userId}` : null
  const collapsedKey = userId ? `treer:tree-collapsed:v1:${userId}` : null
  const max = Math.max(MIN_WIDTH, Math.min(MAX_WIDTH, Math.floor(viewport * 0.45), viewport - 408))
  const width = clamp(preference, max)

  useEffect(() => {
    let value = DEFAULT_WIDTH
    let hidden = false
    try {
      const stored = key ? window.localStorage.getItem(key) : null
      if (stored && Number.isFinite(Number(stored))) value = clamp(Number(stored), MAX_WIDTH)
      hidden = collapsedKey ? window.localStorage.getItem(collapsedKey) === "true" : false
    } catch { /* Browser storage is optional. */ }
    setPreference(value)
    setCollapsed(hidden)
  }, [key, collapsedKey])

  useEffect(() => {
    if (!restoreFocus.current) return
    restoreFocus.current = false
    ;(collapsed ? expandRef : collapseRef).current?.focus()
  }, [collapsed])

  function finish() {
    if (!drag.current) return
    drag.current = null
    setDragging(false)
    finishRef.current()
  }
  useEffect(() => {
    const resize = () => { setViewport(window.innerWidth); if (window.innerWidth < 768) finish() }
    window.addEventListener("resize", resize)
    window.addEventListener("blur", finish)
    return () => { window.removeEventListener("resize", resize); window.removeEventListener("blur", finish) }
  }, [])

  function update(value: number) {
    const next = clamp(value, max)
    setPreference(next)
    try { if (key) window.localStorage.setItem(key, String(next)) } catch { /* Keep in-memory resizing. */ }
  }
  return {
    width, max, dragging, collapsed, collapseRef, expandRef,
    toggle() {
      finish()
      restoreFocus.current = true
      const next = !collapsed
      setCollapsed(next)
      try { if (collapsedKey) window.localStorage.setItem(collapsedKey, String(next)) } catch { /* Keep in-memory toggling. */ }
      finishRef.current()
    },
    style: { "--tree-width": collapsed ? "0px" : `${width}px`, "--tree-divider": collapsed ? "0px" : "1px" } as CSSProperties,
    separator: {
      role: "separator", tabIndex: 0, "aria-label": t("resizer.resize_tree_sidebar"), "aria-orientation": "vertical" as const,
      "aria-controls": "tree-sidebar", "aria-valuemin": MIN_WIDTH, "aria-valuemax": max, "aria-valuenow": width,
      "aria-valuetext": t("resizer.pixels", { width }),
      onPointerDown(event: PointerEvent<HTMLDivElement>) {
        if (viewport < 768 || !event.isPrimary || event.button !== 0) return
        event.preventDefault()
        event.currentTarget.focus()
        event.currentTarget.setPointerCapture(event.pointerId)
        drag.current = { id: event.pointerId, x: event.clientX, width }
        setDragging(true)
      },
      onPointerMove(event: PointerEvent<HTMLDivElement>) {
        if (drag.current?.id === event.pointerId) update(drag.current.width + event.clientX - drag.current.x)
      },
      onPointerUp(event: PointerEvent<HTMLDivElement>) { if (drag.current?.id === event.pointerId) finish() },
      onPointerCancel(event: PointerEvent<HTMLDivElement>) { if (drag.current?.id === event.pointerId) finish() },
      onLostPointerCapture(event: PointerEvent<HTMLDivElement>) { if (drag.current?.id === event.pointerId) finish() },
      onDoubleClick() { update(DEFAULT_WIDTH); finishRef.current() },
      onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
        const step = event.shiftKey ? 64 : 16
        const value = { ArrowLeft: width - step, ArrowRight: width + step, Home: MIN_WIDTH, End: max }[event.key]
        if (value === undefined) return
        event.preventDefault(); update(value); finishRef.current()
      },
    },
  }
}

// One visible 1px line; the wider invisible zone around it is the drag/keyboard handle.
export function TreeResizer({ settings }: { settings: ReturnType<typeof useTreeWidth> }) {
  return <div className="relative hidden min-h-0 bg-border md:block">
    {!settings.collapsed && <div {...settings.separator} className="absolute inset-y-0 left-1/2 z-20 w-6 -translate-x-1/2 touch-none cursor-col-resize outline-none before:absolute before:inset-y-0 before:left-1/2 before:w-px before:-translate-x-1/2 hover:before:w-0.5 hover:before:bg-primary/60 focus-visible:before:w-0.5 focus-visible:before:bg-ring" />}
    {settings.dragging && <div aria-hidden="true" className="fixed inset-0 z-[200] cursor-col-resize touch-none" />}
  </div>
}
